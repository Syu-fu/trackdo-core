//! Replicas exchanging operations through a simulated server must converge,
//! regardless of how local edits, sends, deliveries and replays interleave.

use std::cell::Cell;
use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;

use proptest::prelude::*;

use trackdo_core::env::{Clock, IdGen};
use trackdo_core::hlc::{DeviceId, Hlc};
use trackdo_core::lww::Lww;
use trackdo_core::op::{Op, OpId, OpTarget, Seq};
use trackdo_core::replica::{CommandError, Replica};
use trackdo_core::task::{Task, TaskId};

const REPLICAS: usize = 3;

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

    fn op_id(&mut self) -> OpId {
        OpId(self.fresh())
    }
}

/// Assigns sequence numbers in arrival order and drops retransmissions.
#[derive(Default)]
struct Server {
    log: Vec<Op>,
    seen: HashSet<OpId>,
}

impl Server {
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

    fn snapshot(&self) -> (Vec<Task>, Option<Seq>) {
        let seq = (!self.log.is_empty()).then_some(Seq(self.log.len() as u64));
        (replay_log(self), seq)
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
        let (tasks, seq) = server.snapshot();
        self.replica.apply_snapshot(&tasks, seq).unwrap();
        for hlc in tasks.iter().flat_map(register_hlcs) {
            self.observe(hlc);
        }
    }

    fn pick_task(&self, index: usize) -> Option<TaskId> {
        let ids: Vec<_> = self.replica.tasks().map(Task::id).collect();
        (!ids.is_empty()).then(|| ids[index % ids.len()])
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
            self.replica.apply_remote(&op, seq).unwrap();
            self.observe(op.hlc);
        }
    }

    fn observe(&mut self, hlc: Hlc) {
        self.max_seen = self.max_seen.max(Some(hlc));
    }

    fn snapshot(&self) -> Vec<Task> {
        self.replica.tasks().cloned().collect()
    }
}

#[derive(Debug, Clone)]
enum Edit {
    Create(String),
    Rename(usize, String),
    Complete(usize),
    Reopen(usize),
    Delete(usize),
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
    prop_oneof![
        title().prop_map(Edit::Create),
        (any::<usize>(), title()).prop_map(|(i, t)| Edit::Rename(i, t)),
        any::<usize>().prop_map(Edit::Complete),
        any::<usize>().prop_map(Edit::Reopen),
        any::<usize>().prop_map(Edit::Delete),
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

/// Ops emitted by an edit must be newer than everything the node has seen,
/// so an edit made after observing a remote write always wins over it.
fn run_edit(node: &mut Node, edit: &Edit) {
    let before = node.replica.pending_ops().len();
    let result = match edit {
        Edit::Create(title) => node.replica.create(title).map(|_| ()),
        Edit::Rename(i, title) => match node.pick_task(*i) {
            Some(id) => node.replica.rename(id, title),
            None => Ok(()),
        },
        Edit::Complete(i) => node
            .pick_task(*i)
            .map_or(Ok(()), |id| node.replica.complete(id)),
        Edit::Reopen(i) => node
            .pick_task(*i)
            .map_or(Ok(()), |id| node.replica.reopen(id)),
        Edit::Delete(i) => node
            .pick_task(*i)
            .map_or(Ok(()), |id| node.replica.delete(id)),
    };
    match result {
        Ok(()) | Err(CommandError::Deleted(_)) => {}
        Err(other) => panic!("unexpected command error: {other}"),
    }
    let emitted: Vec<Hlc> = node.replica.pending_ops()[before..]
        .iter()
        .map(|op| op.hlc)
        .collect();
    for hlc in emitted {
        assert!(
            Some(hlc) > node.max_seen,
            "emitted {hlc:?} is not newer than observed {:?}",
            node.max_seen
        );
        node.observe(hlc);
    }
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

fn register_hlcs(task: &Task) -> impl Iterator<Item = Hlc> {
    [
        task.title_register().map(Lww::hlc),
        task.status_register().map(Lww::hlc),
        task.deleted_register().map(Lww::hlc),
    ]
    .into_iter()
    .flatten()
}

/// State obtained by applying the server log, in order, to nothing.
fn replay_log(server: &Server) -> Vec<Task> {
    server
        .log
        .iter()
        .fold(BTreeMap::new(), |mut tasks, op| {
            match &op.target {
                OpTarget::Task { task_id, change } => {
                    let task = tasks
                        .remove(task_id)
                        .unwrap_or_else(|| Task::empty(*task_id));
                    tasks.insert(*task_id, task.apply(change, op.hlc));
                }
            }
            tasks
        })
        .into_values()
        .collect()
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
        let mut server = Server::default();

        for action in &actions {
            run(&mut nodes[target(action)], &mut server, action);
        }

        // Quiesce: everyone sends what is pending, then catches up.
        for node in &mut nodes {
            let acked = server.push(node.replica.pending_ops());
            node.replica.acknowledge(&acked);
        }
        for node in &mut nodes {
            node.pull(&server, usize::MAX);
        }

        let expected = replay_log(&server);
        for node in &nodes {
            prop_assert!(node.replica.pending_ops().is_empty());
            prop_assert_eq!(node.snapshot(), expected.clone());
        }
    }
}
