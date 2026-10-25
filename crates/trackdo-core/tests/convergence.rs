//! Replicas exchanging operations through a simulated server must converge,
//! regardless of how local edits, sends, deliveries and replays interleave.
//! At every step at most one timer is shown running, and never one on a
//! deleted task.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::rc::Rc;

use proptest::prelude::*;

use trackdo_core::date::Date;
use trackdo_core::env::{Clock, IdGen};
use trackdo_core::hlc::{DeviceId, Hlc};
use trackdo_core::lww::Lww;
use trackdo_core::op::{LogId, Op, OpId, OpTarget, Seq};
use trackdo_core::replica::{CommandError, Replica, Snapshot};
use trackdo_core::tag::{Tag, TagId};
use trackdo_core::task::{Task, TaskId, TaskPriority};
use trackdo_core::time_entry::{TimeEntry, TimeEntryId, Timestamp};

const REPLICAS: usize = 3;
/// Rounds of sending and receiving after which healing must have settled.
const QUIESCE_ROUNDS: usize = 4;

#[derive(Clone)]
struct SimClock(Rc<Cell<u64>>);

impl Clock for SimClock {
    fn now_millis(&self) -> u64 {
        self.0.get()
    }
}

/// Ids are prefixed with the device so replicas never collide. The counter
/// lives outside the replica so it survives simulated restarts.
struct DeviceIds {
    device: u128,
    next: Rc<Cell<u128>>,
}

impl DeviceIds {
    fn fresh(&mut self) -> u128 {
        self.next.set(self.next.get() + 1);
        (self.device << 64) | self.next.get()
    }
}

impl IdGen for DeviceIds {
    fn task_id(&mut self) -> TaskId {
        TaskId(self.fresh())
    }

    fn time_entry_id(&mut self) -> TimeEntryId {
        TimeEntryId(self.fresh())
    }

    fn tag_id(&mut self) -> TagId {
        TagId(self.fresh())
    }

    fn op_id(&mut self) -> OpId {
        OpId(self.fresh())
    }
}

/// Assigns sequence numbers in arrival order and drops retransmissions.
struct Server {
    log_id: LogId,
    log: Vec<Op>,
    seen: HashSet<OpId>,
}

impl Server {
    fn new() -> Self {
        Self {
            log_id: LogId(1),
            log: Vec::new(),
            seen: HashSet::new(),
        }
    }

    /// Stores new ops and returns the ids of every op it now holds,
    /// including retransmitted ones, as the `POST /ops` response.
    fn push(&mut self, ops: &[Op]) -> Vec<OpId> {
        for op in ops {
            if self.seen.insert(op.op_id) {
                self.log.push(op.clone());
            }
        }
        ops.iter().map(|op| op.op_id).collect()
    }

    /// Simulates restoring an older backup: the log loses its newest ops and
    /// becomes a new log.
    fn roll_back(&mut self, keep: usize) {
        self.log.truncate(keep % (self.log.len() + 1));
        self.seen = self.log.iter().map(|op| op.op_id).collect();
        self.log_id = LogId(self.log_id.0 + 1);
    }

    fn snapshot(&self) -> Snapshot {
        let (tasks, time_entries, tags) = replay_log(self);
        let seq = (!self.log.is_empty()).then_some(Seq(self.log.len() as u64));
        Snapshot {
            log_id: self.log_id,
            tasks,
            time_entries,
            tags,
            seq,
        }
    }

    fn events_after(&self, last: Option<Seq>) -> impl Iterator<Item = (Seq, &Op)> {
        let start = last.map_or(0, |Seq(seq)| seq as usize);
        self.log
            .iter()
            .enumerate()
            .skip(start)
            .map(|(index, op)| (Seq(index as u64 + 1), op))
    }
}

struct Node {
    device: u128,
    clock: Rc<Cell<u64>>,
    next_id: Rc<Cell<u128>>,
    replica: Replica<SimClock, DeviceIds>,
    /// Newest timestamp this node has observed, local or remote.
    max_seen: Option<Hlc>,
}

impl Node {
    fn new(device: usize, skew_ms: u64) -> Self {
        let clock = Rc::new(Cell::new(1_000_000 + skew_ms));
        let next_id = Rc::new(Cell::new(0));
        let device = device as u128 + 1;
        let replica = Replica::new(
            DeviceId(device),
            SimClock(Rc::clone(&clock)),
            DeviceIds {
                device,
                next: Rc::clone(&next_id),
            },
        );
        Self {
            device,
            clock,
            next_id,
            replica,
            max_seen: None,
        }
    }

    /// Simulates an app restart: only the persisted state survives.
    fn restart(&mut self) {
        self.replica = Replica::restore(
            self.replica.state(),
            SimClock(Rc::clone(&self.clock)),
            DeviceIds {
                device: self.device,
                next: Rc::clone(&self.next_id),
            },
        );
    }

    fn resync(&mut self, server: &Server) {
        let snapshot = server.snapshot();
        let task_hlcs = snapshot.tasks.iter().flat_map(task_hlcs);
        let entry_hlcs = snapshot.time_entries.iter().flat_map(entry_hlcs);
        let tag_hlcs = snapshot.tags.iter().flat_map(tag_hlcs);
        let observed: Vec<Hlc> = task_hlcs.chain(entry_hlcs).chain(tag_hlcs).collect();
        self.emitting(
            |replica| replica.apply_snapshot(&snapshot).unwrap(),
            observed,
        );
    }

    fn pick_task(&self, index: usize) -> Option<TaskId> {
        pick(self.replica.tasks().map(Task::id), index)
    }

    fn pick_tag(&self, index: usize) -> Option<TagId> {
        pick(self.replica.tags().map(Tag::id), index)
    }

    fn pick_entry(&self, index: usize) -> Option<TimeEntryId> {
        pick(self.replica.time_entries().map(TimeEntry::id), index)
    }

    fn ago(&self, millis: u64) -> Timestamp {
        Timestamp(self.clock.get() - millis)
    }

    /// Resyncs if the server would refuse to stream, as it does when the
    /// replica's position is in another log (or in none it knows of).
    /// Returns whether streaming may go ahead.
    fn resume(&mut self, server: &Server) -> bool {
        let resumable = self.replica.log_id() == Some(server.log_id);
        if !resumable {
            self.resync(server);
        }
        resumable
    }

    fn pull(&mut self, server: &Server, max: usize) {
        if !self.resume(server) {
            return;
        }
        let events: Vec<_> = server
            .events_after(self.replica.last_seq())
            .take(max)
            .map(|(seq, op)| (seq, op.clone()))
            .collect();
        self.deliver(events);
    }

    /// Replays already-delivered events, as after an SSE reconnect.
    fn replay(&mut self, server: &Server, count: usize) {
        if !self.resume(server) {
            return;
        }
        let events: Vec<_> = server
            .events_after(None)
            .take_while(|(seq, _)| Some(*seq) <= self.replica.last_seq())
            .take(count)
            .map(|(seq, op)| (seq, op.clone()))
            .collect();
        self.deliver(events);
    }

    fn deliver(&mut self, events: Vec<(Seq, Op)>) {
        for (seq, op) in events {
            self.emitting(|replica| replica.apply_remote(&op, seq).unwrap(), [op.hlc]);
        }
    }

    /// Runs `step`, which may emit ops (commands, or healing after receiving
    /// `received`). Emitted ops must be newer than everything the node has
    /// seen, so a write made after observing another always wins over it.
    /// The exception is a write held before `step` sent again unchanged
    /// after the server's log was replaced.
    fn emitting<T>(
        &mut self,
        step: impl FnOnce(&mut Replica<SimClock, DeviceIds>) -> T,
        received: impl IntoIterator<Item = Hlc>,
    ) -> T {
        let before: HashSet<OpId> = self
            .replica
            .pending_ops()
            .iter()
            .map(|op| op.op_id)
            .collect();
        let held: HashSet<Hlc> = self.held_hlcs().collect();
        let result = step(&mut self.replica);
        for hlc in received {
            self.observe(hlc);
        }
        let emitted: Vec<Hlc> = self
            .replica
            .pending_ops()
            .iter()
            .filter(|op| !before.contains(&op.op_id))
            .map(|op| op.hlc)
            .collect();
        for hlc in emitted {
            assert!(
                Some(hlc) > self.max_seen || held.contains(&hlc),
                "emitted {hlc:?} is not newer than observed {:?}",
                self.max_seen
            );
            self.observe(hlc);
        }
        result
    }

    fn held_hlcs(&self) -> impl Iterator<Item = Hlc> + '_ {
        let tasks = self.replica.tasks().flat_map(task_hlcs);
        let entries = self.replica.time_entries().flat_map(entry_hlcs);
        let tags = self.replica.tags().flat_map(tag_hlcs);
        tasks.chain(entries).chain(tags)
    }

    fn observe(&mut self, hlc: Hlc) {
        self.max_seen = self.max_seen.max(Some(hlc));
    }

    fn snapshot(&self) -> (Vec<Task>, Vec<TimeEntry>, Vec<Tag>) {
        (
            self.replica.tasks().cloned().collect(),
            self.replica.time_entries().cloned().collect(),
            self.replica.tags().cloned().collect(),
        )
    }

    /// Every task with an entry, including tasks not received yet.
    fn tracked_tasks(&self) -> BTreeSet<TaskId> {
        self.replica
            .time_entries()
            .map(TimeEntry::task_id)
            .collect()
    }

    /// What the user sees: at most one running timer, never on a deleted task.
    fn check_shown_timers(&self) {
        let running: Vec<_> = self
            .tracked_tasks()
            .into_iter()
            .flat_map(|task| self.replica.entries_for(task))
            .filter(|view| view.is_running())
            .collect();
        assert!(
            running.len() <= 1,
            "several timers shown running: {running:?}"
        );
        let tree = self.replica.hierarchy();
        for view in running {
            assert!(
                !tree.is_deleted(view.task_id()),
                "timer shown on deleted task"
            );
        }
    }

    /// What is stored once healing settled: the same as what is shown.
    fn check_stored_timers(&self) {
        let running: Vec<_> = self
            .replica
            .time_entries()
            .filter(|entry| entry.is_running() && !entry.is_deleted())
            .collect();
        assert!(
            running.len() <= 1,
            "several timers stored running: {running:?}"
        );
        let tree = self.replica.hierarchy();
        for entry in running {
            assert!(
                !tree.is_deleted(entry.task_id()),
                "timer stored on deleted task"
            );
        }
    }
}

#[derive(Debug, Clone)]
enum Edit {
    Create(String),
    Rename(usize, String),
    Complete(usize),
    Reopen(usize),
    Delete(usize),
    SetPriority(usize, TaskPriority),
    /// The second index picks the new parent; `None` moves to the top level.
    Move(usize, Option<usize>),
    SetNote(usize, String),
    /// Day of October 2026, or `None` to clear the due date.
    SetDue(usize, Option<u8>),
    /// Milliseconds from now, or `None` to clear the scheduled time.
    Schedule(usize, Option<u64>),
    Start(usize),
    Stop(usize),
    /// Start and optional stop, as milliseconds before now.
    SetTimes(usize, u64, Option<u64>),
    DeleteEntry(usize),
    /// Two instants before now; the earlier one becomes the start.
    AddEntry(usize, u64, u64),
    CreateTag(String),
    RenameTag(usize, String),
    DeleteTag(usize),
}

#[derive(Debug, Clone)]
enum Action {
    Edit(usize, Edit),
    /// The bool tells whether the response reaches the client.
    Push(usize, bool),
    Pull(usize, usize),
    Replay(usize, usize),
    Tick(usize, u64),
    Resync(usize),
    Restart(usize),
    /// The server restores a backup holding this many of its ops (modulo the
    /// log length).
    RollBack(usize),
}

fn title() -> impl Strategy<Value = String> {
    prop::sample::select(vec!["milk", "eggs", "bread", "call mom", "牛乳"]).prop_map(String::from)
}

/// Few names, so devices create the same tag and renames collide.
fn tag_name() -> impl Strategy<Value = String> {
    prop::sample::select(vec!["work", "home", "errand"]).prop_map(String::from)
}

fn priority() -> impl Strategy<Value = TaskPriority> {
    prop::sample::select(vec![
        TaskPriority::None,
        TaskPriority::Low,
        TaskPriority::Medium,
        TaskPriority::High,
    ])
}

fn edit() -> impl Strategy<Value = Edit> {
    let i = any::<usize>;
    let ago = || 0u64..3_000;
    prop_oneof![
        1 => title().prop_map(Edit::Create),
        1 => (i(), title()).prop_map(|(i, t)| Edit::Rename(i, t)),
        1 => i().prop_map(Edit::Complete),
        1 => i().prop_map(Edit::Reopen),
        1 => i().prop_map(Edit::Delete),
        1 => (i(), priority()).prop_map(|(i, p)| Edit::SetPriority(i, p)),
        3 => (i(), prop::option::of(i())).prop_map(|(i, parent)| Edit::Move(i, parent)),
        1 => (i(), prop::sample::select(vec!["", "call first", "a\nb"]))
            .prop_map(|(i, n)| Edit::SetNote(i, n.to_owned())),
        1 => (i(), prop::option::of(1u8..4)).prop_map(|(i, day)| Edit::SetDue(i, day)),
        1 => (i(), prop::option::of(ago())).prop_map(|(i, ahead)| Edit::Schedule(i, ahead)),
        3 => i().prop_map(Edit::Start),
        1 => i().prop_map(Edit::Stop),
        1 => (i(), ago(), prop::option::of(ago()))
            .prop_map(|(i, start, stop)| Edit::SetTimes(i, start, stop)),
        1 => i().prop_map(Edit::DeleteEntry),
        1 => (i(), ago(), ago()).prop_map(|(i, a, b)| Edit::AddEntry(i, a, b)),
        1 => tag_name().prop_map(Edit::CreateTag),
        1 => (i(), tag_name()).prop_map(|(i, name)| Edit::RenameTag(i, name)),
        1 => i().prop_map(Edit::DeleteTag),
    ]
}

fn action() -> impl Strategy<Value = Action> {
    let node = 0..REPLICAS;
    prop_oneof![
        4 => (node.clone(), edit()).prop_map(|(n, e)| Action::Edit(n, e)),
        2 => (node.clone(), any::<bool>()).prop_map(|(n, ok)| Action::Push(n, ok)),
        2 => (node.clone(), 1usize..5).prop_map(|(n, max)| Action::Pull(n, max)),
        1 => (node.clone(), 1usize..5).prop_map(|(n, count)| Action::Replay(n, count)),
        1 => (node.clone(), 0u64..3).prop_map(|(n, delta)| Action::Tick(n, delta)),
        1 => node.clone().prop_map(Action::Resync),
        1 => node.prop_map(Action::Restart),
        1 => any::<usize>().prop_map(Action::RollBack),
    ]
}

fn run_edit(node: &mut Node, edit: &Edit) {
    let task = node.pick_task(edit_index(edit));
    let entry = node.pick_entry(edit_index(edit));
    let tag = node.pick_tag(edit_index(edit));
    let result = match edit {
        Edit::Create(title) => node.emitting(|r| r.create(title).map(drop), []),
        Edit::CreateTag(name) => node.emitting(|r| r.create_tag(name).map(drop), []),
        Edit::RenameTag(_, name) => on(node, tag, |r, id| r.rename_tag(id, name)),
        Edit::DeleteTag(_) => on(node, tag, Replica::delete_tag),
        Edit::Rename(_, title) => on(node, task, |r, id| r.rename(id, title)),
        Edit::Complete(_) => on(node, task, Replica::complete),
        Edit::Reopen(_) => on(node, task, Replica::reopen),
        Edit::Delete(_) => on(node, task, Replica::delete),
        Edit::SetPriority(_, priority) => on(node, task, |r, id| r.set_priority(id, *priority)),
        Edit::Move(_, parent) => match parent.map(|index| node.pick_task(index)) {
            Some(None) => Ok(()),
            Some(Some(parent)) => on(node, task, |r, id| r.set_parent(id, Some(parent))),
            None => on(node, task, |r, id| r.set_parent(id, None)),
        },
        Edit::SetNote(_, note) => on(node, task, |r, id| r.set_note(id, note)),
        Edit::Schedule(_, ahead) => {
            let at = ahead.map(|ahead| Timestamp(node.ago(0).0 + ahead));
            on(node, task, |r, id| r.schedule(id, at))
        }
        Edit::SetDue(_, day) => {
            let due = day.map(|day| Date::new(2026, 10, day).unwrap());
            on(node, task, |r, id| r.set_due(id, due))
        }
        Edit::Start(_) => on(node, task, |r, id| r.start(id).map(drop)),
        Edit::Stop(_) => on(node, entry, Replica::stop),
        Edit::SetTimes(_, start, stop) => {
            let (start, stop) = (node.ago(*start), stop.map(|stop| node.ago(stop)));
            on(node, entry, |r, id| r.edit_times(id, start, stop))
        }
        Edit::DeleteEntry(_) => on(node, entry, Replica::delete_entry),
        Edit::AddEntry(_, a, b) => {
            let (start, stop) = (node.ago(*a.max(b)), node.ago(*a.min(b)));
            on(node, task, |r, id| r.add_entry(id, start, stop).map(drop))
        }
    };
    match result {
        Ok(())
        | Err(
            CommandError::Deleted(_)
            | CommandError::EntryDeleted(_)
            | CommandError::NotRunning(_)
            | CommandError::StopBeforeStart { .. }
            | CommandError::Cycle { .. }
            | CommandError::TagDeleted(_)
            | CommandError::TagNameTaken(_),
        ) => {}
        // An entry may arrive before its task does.
        Err(CommandError::NotFound(_))
            if matches!(
                edit,
                Edit::Stop(_) | Edit::SetTimes(..) | Edit::DeleteEntry(_)
            ) => {}
        Err(other) => panic!("unexpected command error: {other}"),
    }
}

fn edit_index(edit: &Edit) -> usize {
    match edit {
        Edit::Create(_) | Edit::CreateTag(_) => 0,
        Edit::Rename(i, _)
        | Edit::Complete(i)
        | Edit::Reopen(i)
        | Edit::Delete(i)
        | Edit::SetPriority(i, _)
        | Edit::Move(i, _)
        | Edit::SetNote(i, _)
        | Edit::SetDue(i, _)
        | Edit::Schedule(i, _)
        | Edit::Start(i)
        | Edit::Stop(i)
        | Edit::SetTimes(i, ..)
        | Edit::DeleteEntry(i)
        | Edit::AddEntry(i, ..)
        | Edit::RenameTag(i, _)
        | Edit::DeleteTag(i) => *i,
    }
}

/// Runs `command` on `id`, or does nothing when there was nothing to pick.
fn on<I>(
    node: &mut Node,
    id: Option<I>,
    command: impl FnOnce(&mut Replica<SimClock, DeviceIds>, I) -> Result<(), CommandError>,
) -> Result<(), CommandError> {
    match id {
        Some(id) => node.emitting(|replica| command(replica, id), []),
        None => Ok(()),
    }
}

fn pick<T>(ids: impl Iterator<Item = T>, index: usize) -> Option<T> {
    let ids: Vec<T> = ids.collect();
    let len = ids.len();
    ids.into_iter().nth(index % len.max(1))
}

fn run(node: &mut Node, server: &mut Server, action: &Action) {
    match action {
        Action::Edit(_, edit) => run_edit(node, edit),
        Action::Push(_, response_delivered) => {
            let acked = server.push(node.replica.pending_ops());
            if *response_delivered {
                node.replica.acknowledge(&acked);
            }
        }
        Action::Pull(_, max) => node.pull(server, *max),
        Action::Replay(_, count) => node.replay(server, *count),
        Action::Tick(_, delta) => node.clock.set(node.clock.get() + delta),
        Action::Resync(_) => node.resync(server),
        Action::Restart(_) => node.restart(),
        Action::RollBack(keep) => server.roll_back(*keep),
    }
}

fn target(action: &Action) -> usize {
    match action {
        Action::Edit(n, _)
        | Action::Push(n, _)
        | Action::Pull(n, _)
        | Action::Replay(n, _)
        | Action::Tick(n, _)
        | Action::Resync(n)
        | Action::Restart(n) => *n,
        Action::RollBack(_) => 0,
    }
}

fn task_hlcs(task: &Task) -> impl Iterator<Item = Hlc> {
    [
        task.registers().title.as_ref().map(Lww::hlc),
        task.registers().status.as_ref().map(Lww::hlc),
        task.registers().deleted.as_ref().map(Lww::hlc),
        task.registers().priority.as_ref().map(Lww::hlc),
        task.registers().note.as_ref().map(Lww::hlc),
        task.registers().due.as_ref().map(Lww::hlc),
        task.registers().scheduled_at.as_ref().map(Lww::hlc),
        task.registers().parent.as_ref().map(Lww::hlc),
    ]
    .into_iter()
    .flatten()
}

fn entry_hlcs(entry: &TimeEntry) -> impl Iterator<Item = Hlc> {
    [
        entry.started_at_register().map(Lww::hlc),
        entry.stopped_at_register().map(Lww::hlc),
        entry.deleted_register().map(Lww::hlc),
    ]
    .into_iter()
    .flatten()
}

fn tag_hlcs(tag: &Tag) -> impl Iterator<Item = Hlc> {
    [
        tag.registers().name.as_ref().map(Lww::hlc),
        tag.registers().deleted.as_ref().map(Lww::hlc),
    ]
    .into_iter()
    .flatten()
}

/// State obtained by applying the server log, in order, to nothing.
fn replay_log(server: &Server) -> (Vec<Task>, Vec<TimeEntry>, Vec<Tag>) {
    let mut tasks = BTreeMap::new();
    let mut entries = BTreeMap::new();
    let mut tags = BTreeMap::new();
    for op in &server.log {
        match &op.target {
            OpTarget::Task { task_id, change } => {
                let task = tasks
                    .remove(task_id)
                    .unwrap_or_else(|| Task::empty(*task_id));
                tasks.insert(*task_id, task.apply(change, op.hlc));
            }
            OpTarget::TimeEntry {
                entry_id,
                task_id,
                change,
            } => {
                let entry = entries
                    .remove(entry_id)
                    .unwrap_or_else(|| TimeEntry::empty(*entry_id, *task_id));
                entries.insert(*entry_id, entry.apply(change, op.hlc));
            }
            OpTarget::Tag { tag_id, change } => {
                let tag = tags.remove(tag_id).unwrap_or_else(|| Tag::empty(*tag_id));
                tags.insert(*tag_id, tag.apply(change, op.hlc));
            }
        }
    }
    (
        tasks.into_values().collect(),
        entries.into_values().collect(),
        tags.into_values().collect(),
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn replicas_converge(
        skews in prop::array::uniform3(0u64..5_000),
        actions in prop::collection::vec(action(), 0..80),
    ) {
        let mut nodes: Vec<Node> = skews
            .iter()
            .enumerate()
            .map(|(device, skew)| Node::new(device, *skew))
            .collect();
        let mut server = Server::new();

        for action in &actions {
            let node = &mut nodes[target(action)];
            run(node, &mut server, action);
            node.check_shown_timers();
        }

        // Quiesce: everyone sends what is pending, then catches up. Catching
        // up may heal timers, whose stops are sent in the next round.
        for _ in 0..QUIESCE_ROUNDS {
            for node in &mut nodes {
                let acked = server.push(node.replica.pending_ops());
                node.replica.acknowledge(&acked);
            }
            for node in &mut nodes {
                node.pull(&server, usize::MAX);
            }
        }

        let expected = replay_log(&server);
        for node in &nodes {
            prop_assert!(node.replica.pending_ops().is_empty());
            prop_assert_eq!(node.snapshot(), expected.clone());
            node.check_stored_timers();
        }
    }
}
