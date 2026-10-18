//! Replicas exchanging operations through a simulated server must converge,
//! regardless of how local edits, sends, deliveries and replays interleave.
//! At every step at most one timer is shown running, and never one on a
//! deleted task.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::rc::Rc;

use proptest::prelude::*;

use trackdo_core::env::{Clock, IdGen};
use trackdo_core::hlc::{DeviceId, Hlc};
use trackdo_core::lww::Lww;
use trackdo_core::op::{LogId, Op, OpId, OpTarget, Seq};
use trackdo_core::replica::{CommandError, Replica, Snapshot};
use trackdo_core::task::{Task, TaskId};
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

    fn snapshot(&self) -> Snapshot {
        let (tasks, time_entries) = replay_log(self);
        let seq = (!self.log.is_empty()).then_some(Seq(self.log.len() as u64));
        Snapshot {
            log_id: self.log_id,
            tasks,
            time_entries,
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
        let observed: Vec<Hlc> = task_hlcs.chain(entry_hlcs).collect();
        self.emitting(
            |replica| replica.apply_snapshot(&snapshot).unwrap(),
            observed,
        );
    }

    fn pick_task(&self, index: usize) -> Option<TaskId> {
        pick(self.replica.tasks().map(Task::id), index)
    }

    fn pick_entry(&self, index: usize) -> Option<TimeEntryId> {
        pick(self.replica.time_entries().map(TimeEntry::id), index)
    }

    fn ago(&self, millis: u64) -> Timestamp {
        Timestamp(self.clock.get() - millis)
    }

    fn pull(&mut self, server: &Server, max: usize) {
        let events: Vec<_> = server
            .events_after(self.replica.last_seq())
            .take(max)
            .map(|(seq, op)| (seq, op.clone()))
            .collect();
        self.deliver(events);
    }

    /// Replays already-delivered events, as after an SSE reconnect.
    fn replay(&mut self, server: &Server, count: usize) {
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
                Some(hlc) > self.max_seen,
                "emitted {hlc:?} is not newer than observed {:?}",
                self.max_seen
            );
            self.observe(hlc);
        }
        result
    }

    fn observe(&mut self, hlc: Hlc) {
        self.max_seen = self.max_seen.max(Some(hlc));
    }

    fn snapshot(&self) -> (Vec<Task>, Vec<TimeEntry>) {
        (
            self.replica.tasks().cloned().collect(),
            self.replica.time_entries().cloned().collect(),
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
        for view in running {
            let task = self.replica.task(view.task_id());
            assert!(
                !task.is_some_and(Task::is_deleted),
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
        for entry in running {
            let task = self.replica.task(entry.task_id());
            assert!(
                !task.is_some_and(Task::is_deleted),
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
    Start(usize),
    Stop(usize),
    /// Start and optional stop, as milliseconds before now.
    SetTimes(usize, u64, Option<u64>),
    DeleteEntry(usize),
    /// Two instants before now; the earlier one becomes the start.
    AddEntry(usize, u64, u64),
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
}

fn title() -> impl Strategy<Value = String> {
    prop::sample::select(vec!["milk", "eggs", "bread", "call mom", "牛乳"]).prop_map(String::from)
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
        3 => i().prop_map(Edit::Start),
        1 => i().prop_map(Edit::Stop),
        1 => (i(), ago(), prop::option::of(ago()))
            .prop_map(|(i, start, stop)| Edit::SetTimes(i, start, stop)),
        1 => i().prop_map(Edit::DeleteEntry),
        1 => (i(), ago(), ago()).prop_map(|(i, a, b)| Edit::AddEntry(i, a, b)),
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
    ]
}

fn run_edit(node: &mut Node, edit: &Edit) {
    let task = node.pick_task(edit_index(edit));
    let entry = node.pick_entry(edit_index(edit));
    let result = match edit {
        Edit::Create(title) => node.emitting(|r| r.create(title).map(drop), []),
        Edit::Rename(_, title) => on(node, task, |r, id| r.rename(id, title)),
        Edit::Complete(_) => on(node, task, Replica::complete),
        Edit::Reopen(_) => on(node, task, Replica::reopen),
        Edit::Delete(_) => on(node, task, Replica::delete),
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
            | CommandError::StopBeforeStart { .. },
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
        Edit::Create(_) => 0,
        Edit::Rename(i, _)
        | Edit::Complete(i)
        | Edit::Reopen(i)
        | Edit::Delete(i)
        | Edit::Start(i)
        | Edit::Stop(i)
        | Edit::SetTimes(i, ..)
        | Edit::DeleteEntry(i)
        | Edit::AddEntry(i, ..) => *i,
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
    }
}

fn task_hlcs(task: &Task) -> impl Iterator<Item = Hlc> {
    [
        task.title_register().map(Lww::hlc),
        task.status_register().map(Lww::hlc),
        task.deleted_register().map(Lww::hlc),
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

/// State obtained by applying the server log, in order, to nothing.
fn replay_log(server: &Server) -> (Vec<Task>, Vec<TimeEntry>) {
    let mut tasks = BTreeMap::new();
    let mut entries = BTreeMap::new();
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
        }
    }
    (
        tasks.into_values().collect(),
        entries.into_values().collect(),
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
