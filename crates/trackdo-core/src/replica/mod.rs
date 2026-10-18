//! A replica: local state of tasks and time entries plus the operations not
//! yet sent.
//!
//! Commands validate user intent, emit operations and apply them locally at
//! once, so the UI never waits on the network.

mod healing;
mod task_commands;
#[cfg(test)]
mod test_support;
mod time_commands;
mod time_queries;

use std::collections::BTreeMap;

use thiserror::Error;

use crate::env::{Clock, IdGen};
use crate::hlc::{DeviceId, Hlc, HlcError};
use crate::op::{Op, OpId, OpTarget, Seq};
use crate::task::{Task, TaskChange, TaskId};
use crate::time_entry::{TimeEntry, TimeEntryId, Timestamp};
use crate::title::TitleError;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CommandError {
    #[error(transparent)]
    Title(#[from] TitleError),
    #[error("task {0:?} does not exist")]
    NotFound(TaskId),
    #[error("task {0:?} is deleted")]
    Deleted(TaskId),
    #[error("time entry {0:?} does not exist")]
    EntryNotFound(TimeEntryId),
    #[error("time entry {0:?} is deleted")]
    EntryDeleted(TimeEntryId),
    #[error("time entry {0:?} is not running")]
    NotRunning(TimeEntryId),
    #[error("stop time {stopped:?} precedes start time {started:?}")]
    StopBeforeStart {
        started: Timestamp,
        stopped: Timestamp,
    },
    #[error("time {0:?} is in the future")]
    InFuture(Timestamp),
    #[error(transparent)]
    Clock(#[from] HlcError),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RemoteError {
    #[error(transparent)]
    Clock(#[from] HlcError),
    /// The server must never accept such an operation; the replica rejects it
    /// rather than attributing the entry's time to two tasks.
    #[error("time entry {entry:?} belongs to {expected:?}, not {actual:?}")]
    TaskMismatch {
        entry: TimeEntryId,
        expected: TaskId,
        actual: TaskId,
    },
}

/// Everything a host must persist to restore a replica after a restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaState {
    pub hlc: Hlc,
    pub tasks: Vec<Task>,
    pub time_entries: Vec<TimeEntry>,
    pub pending: Vec<Op>,
    pub last_seq: Option<Seq>,
}

pub struct Replica<C, G> {
    clock: C,
    ids: G,
    hlc: Hlc,
    tasks: BTreeMap<TaskId, Task>,
    entries: BTreeMap<TimeEntryId, TimeEntry>,
    pending: Vec<Op>,
    last_seq: Option<Seq>,
}

impl<C: Clock, G: IdGen> Replica<C, G> {
    pub fn new(device: DeviceId, clock: C, ids: G) -> Self {
        Self {
            clock,
            ids,
            hlc: Hlc::zero(device),
            tasks: BTreeMap::new(),
            entries: BTreeMap::new(),
            pending: Vec::new(),
            last_seq: None,
        }
    }

    /// Rebuilds a replica from a state previously returned by [`Self::state`].
    pub fn restore(state: ReplicaState, clock: C, ids: G) -> Self {
        Self {
            clock,
            ids,
            hlc: state.hlc,
            tasks: merge_by_id(state.tasks, Task::id, Task::merge),
            entries: merge_by_id(state.time_entries, TimeEntry::id, TimeEntry::merge),
            pending: state.pending,
            last_seq: state.last_seq,
        }
    }

    pub fn state(&self) -> ReplicaState {
        ReplicaState {
            hlc: self.hlc,
            tasks: self.tasks.values().cloned().collect(),
            time_entries: self.entries.values().cloned().collect(),
            pending: self.pending.clone(),
            last_seq: self.last_seq,
        }
    }

    pub fn task(&self, id: TaskId) -> Option<&Task> {
        self.tasks.get(&id)
    }

    /// All known tasks, including deleted ones, ordered by id.
    pub fn tasks(&self) -> impl Iterator<Item = &Task> {
        self.tasks.values()
    }

    pub fn time_entry(&self, id: TimeEntryId) -> Option<&TimeEntry> {
        self.entries.get(&id)
    }

    /// All known time entries, including deleted ones, ordered by id.
    pub fn time_entries(&self) -> impl Iterator<Item = &TimeEntry> {
        self.entries.values()
    }

    /// Operations applied locally but not yet acknowledged by the server.
    pub fn pending_ops(&self) -> &[Op] {
        &self.pending
    }

    /// Position of the last operation received from the server, used to
    /// resume the event stream.
    pub fn last_seq(&self) -> Option<Seq> {
        self.last_seq
    }

    /// Applies an operation delivered by the server at `seq`.
    ///
    /// Operations at or before [`Self::last_seq`] are ignored. Receiving one
    /// of this replica's own operations acknowledges it, removing it from
    /// [`Self::pending_ops`].
    ///
    /// If the operation leaves timers that must be treated as stopped, stop
    /// operations for them are queued. Should that fail on the clock, the
    /// received operation is still applied and the stops are retried on the
    /// next call.
    pub fn apply_remote(&mut self, op: &Op, seq: Seq) -> Result<(), RemoteError> {
        if self.last_seq.is_some_and(|last| seq <= last) {
            return Ok(());
        }
        self.check_entry_task(&op.target)?;
        let hlc = self.hlc.receive(&op.hlc, self.clock.now_millis())?;

        self.apply_target(&op.target, op.hlc);
        self.pending.retain(|pending| pending.op_id != op.op_id);
        self.hlc = hlc;
        self.last_seq = Some(seq);
        Ok(self.heal()?)
    }

    /// Confirms operations the server reported as stored, typically from the
    /// `POST /ops` response. Needed because an operation may never be echoed
    /// back, e.g. when a snapshot moves [`Self::last_seq`] past it.
    pub fn acknowledge(&mut self, op_ids: &[OpId]) {
        self.pending.retain(|op| !op_ids.contains(&op.op_id));
    }

    /// Merges a full server snapshot taken at `seq`, e.g. on first sync or
    /// after the event log no longer reaches back to [`Self::last_seq`].
    ///
    /// Pending operations are kept: they are re-sent and acknowledged as usual.
    /// Timers left to be treated as stopped are healed as in
    /// [`Self::apply_remote`].
    pub fn apply_snapshot(&mut self, tasks: &[Task], seq: Option<Seq>) -> Result<(), RemoteError> {
        let newest = tasks.iter().filter_map(Task::newest_hlc).max();
        let hlc = match newest {
            Some(newest) => self.hlc.receive(&newest, self.clock.now_millis())?,
            None => self.hlc,
        };

        for task in tasks {
            let merged = self.task_or_empty(task.id()).merge(task);
            self.tasks.insert(task.id(), merged);
        }
        self.hlc = hlc;
        self.last_seq = self.last_seq.max(seq);
        Ok(self.heal()?)
    }

    /// Writes may target a task this replica has not seen yet.
    fn task_or_empty(&self, id: TaskId) -> Task {
        self.tasks
            .get(&id)
            .cloned()
            .unwrap_or_else(|| Task::empty(id))
    }

    fn entry_or_empty(&self, id: TimeEntryId, task_id: TaskId) -> TimeEntry {
        self.entries
            .get(&id)
            .cloned()
            .unwrap_or_else(|| TimeEntry::empty(id, task_id))
    }

    fn ensure_editable(&self, id: TaskId) -> Result<(), CommandError> {
        match self.tasks.get(&id) {
            None => Err(CommandError::NotFound(id)),
            Some(task) if task.is_deleted() => Err(CommandError::Deleted(id)),
            Some(_) => Ok(()),
        }
    }

    fn check_entry_task(&self, target: &OpTarget) -> Result<(), RemoteError> {
        let OpTarget::TimeEntry {
            entry_id, task_id, ..
        } = target
        else {
            return Ok(());
        };
        match self.entries.get(entry_id) {
            Some(entry) if entry.task_id() != *task_id => Err(RemoteError::TaskMismatch {
                entry: *entry_id,
                expected: entry.task_id(),
                actual: *task_id,
            }),
            _ => Ok(()),
        }
    }

    fn apply_target(&mut self, target: &OpTarget, hlc: Hlc) {
        match target {
            OpTarget::Task { task_id, change } => {
                let updated = self.task_or_empty(*task_id).apply(change, hlc);
                self.tasks.insert(*task_id, updated);
            }
            OpTarget::TimeEntry {
                entry_id,
                task_id,
                change,
            } => {
                let updated = self.entry_or_empty(*entry_id, *task_id).apply(change, hlc);
                self.entries.insert(*entry_id, updated);
            }
        }
    }

    fn emit_task(&mut self, task_id: TaskId, changes: Vec<TaskChange>) -> Result<(), CommandError> {
        self.emit(
            changes
                .into_iter()
                .map(|change| OpTarget::Task { task_id, change })
                .collect(),
        )
    }

    /// Stamps `targets`, applies them locally and queues them for sending.
    /// All timestamps are computed first so a clock failure changes nothing.
    fn emit<E: From<HlcError>>(&mut self, targets: Vec<OpTarget>) -> Result<(), E> {
        let now = self.clock.now_millis();
        let mut hlc = self.hlc;
        let mut stamped = Vec::with_capacity(targets.len());
        for target in targets {
            hlc = hlc.tick(now)?;
            stamped.push((target, hlc));
        }

        self.hlc = hlc;
        for (target, hlc) in stamped {
            self.apply_target(&target, hlc);
            let op_id = self.ids.op_id();
            self.pending.push(Op { op_id, hlc, target });
        }
        Ok(())
    }
}

/// Collects `items` by id, merging duplicates.
fn merge_by_id<K: Ord, V>(
    items: Vec<V>,
    id: impl Fn(&V) -> K,
    merge: impl Fn(&V, &V) -> V,
) -> BTreeMap<K, V> {
    items.into_iter().fold(BTreeMap::new(), |mut merged, item| {
        let item = match merged.remove(&id(&item)) {
            Some(existing) => merge(&item, &existing),
            None => item,
        };
        merged.insert(id(&item), item);
        merged
    })
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use crate::task::TaskStatus;

    fn remote_op(op_id: u128, task_id: TaskId, wall: u64, change: TaskChange) -> Op {
        Op::task(OpId(op_id), task_id, Hlc::new(wall, 0, OTHER), change)
    }

    #[test]
    fn apply_remote_creates_unknown_task() {
        let mut replica = replica();
        let id = TaskId(500);

        replica
            .apply_remote(
                &remote_op(1, id, NOW, TaskChange::Title(title("remote"))),
                Seq(1),
            )
            .unwrap();

        assert_eq!(replica.task(id).unwrap().title(), Some(&title("remote")));
        assert_eq!(replica.last_seq(), Some(Seq(1)));
        assert!(replica.pending_ops().is_empty());
    }

    #[test]
    fn apply_remote_ignores_stale_write() {
        let mut replica = replica();
        let id = replica.create("local").unwrap();

        replica
            .apply_remote(
                &remote_op(900, id, NOW - 1, TaskChange::Title(title("stale"))),
                Seq(1),
            )
            .unwrap();

        assert_eq!(replica.task(id).unwrap().title(), Some(&title("local")));
        assert_eq!(replica.last_seq(), Some(Seq(1)));
    }

    #[test]
    fn apply_remote_skips_already_seen_seq() {
        let mut replica = replica();
        let id = TaskId(500);
        replica
            .apply_remote(
                &remote_op(1, id, NOW, TaskChange::Title(title("a"))),
                Seq(5),
            )
            .unwrap();

        replica
            .apply_remote(
                &remote_op(2, id, NOW + 1, TaskChange::Title(title("b"))),
                Seq(5),
            )
            .unwrap();
        replica
            .apply_remote(
                &remote_op(3, id, NOW + 2, TaskChange::Title(title("c"))),
                Seq(4),
            )
            .unwrap();

        assert_eq!(replica.task(id).unwrap().title(), Some(&title("a")));
        assert_eq!(replica.last_seq(), Some(Seq(5)));
    }

    #[test]
    fn echoed_own_op_is_acknowledged() {
        let mut replica = replica();
        let id = replica.create("a").unwrap();
        let ops = replica.pending_ops().to_vec();

        replica.apply_remote(&ops[0], Seq(1)).unwrap();

        assert_eq!(replica.pending_ops(), &ops[1..]);
        assert_eq!(replica.task(id).unwrap().title(), Some(&title("a")));
    }

    #[test]
    fn remote_op_applies_to_deleted_task() {
        let mut replica = replica();
        let id = replica.create("a").unwrap();
        replica.delete(id).unwrap();

        replica
            .apply_remote(
                &remote_op(900, id, NOW + 10, TaskChange::Status(TaskStatus::Done)),
                Seq(1),
            )
            .unwrap();

        let task = replica.task(id).unwrap();
        assert!(task.is_deleted());
        assert_eq!(task.status(), TaskStatus::Done);
    }

    #[test]
    fn local_writes_after_receive_are_newer_than_remote_write() {
        let mut replica = replica();
        let id = replica.create("a").unwrap();
        let future = NOW + 60_000;
        replica
            .apply_remote(
                &remote_op(900, id, future, TaskChange::Title(title("remote"))),
                Seq(1),
            )
            .unwrap();

        replica.rename(id, "local").unwrap();

        let last = replica.pending_ops().last().unwrap();
        assert!(last.hlc > Hlc::new(future, 0, OTHER));
        assert_eq!(replica.task(id).unwrap().title(), Some(&title("local")));
    }

    #[test]
    fn apply_remote_clock_overflow_changes_nothing() {
        let mut replica = replica();
        let id = TaskId(500);
        let op = Op::task(
            OpId(900),
            id,
            Hlc::new(NOW, u32::MAX, OTHER),
            TaskChange::Title(title("a")),
        );

        let result = replica.apply_remote(&op, Seq(1));

        assert_eq!(result, Err(RemoteError::Clock(HlcError::CounterOverflow)));
        assert!(replica.task(id).is_none());
        assert_eq!(replica.last_seq(), None);
    }

    fn snapshot_task(id: TaskId, wall: u64, change: TaskChange) -> Task {
        Task::empty(id).apply(&change, Hlc::new(wall, 0, OTHER))
    }

    #[test]
    fn state_roundtrips_through_restore() {
        let mut replica = replica();
        let id = replica.create("a").unwrap();
        replica
            .apply_remote(
                &remote_op(900, id, NOW + 5, TaskChange::Status(TaskStatus::Done)),
                Seq(3),
            )
            .unwrap();
        replica.rename(id, "b").unwrap();
        let state = replica.state();

        let restored = Replica::restore(state.clone(), FixedClock(NOW), SequentialIds(1_000));

        assert_eq!(restored.state(), state);
        assert_eq!(restored.last_seq(), Some(Seq(3)));
        assert_eq!(restored.task(id).unwrap().title(), Some(&title("b")));
        assert_eq!(restored.pending_ops(), replica.pending_ops());
    }

    #[test]
    fn restored_replica_keeps_clock_monotonic() {
        let mut replica = replica();
        let id = replica.create("a").unwrap();
        let last_hlc = replica.pending_ops().last().unwrap().hlc;

        let mut restored =
            Replica::restore(replica.state(), FixedClock(NOW - 500), SequentialIds(1_000));
        restored.rename(id, "b").unwrap();

        assert!(restored.pending_ops().last().unwrap().hlc > last_hlc);
    }

    #[test]
    fn apply_snapshot_merges_field_by_field() {
        let mut replica = replica();
        let local = replica.create("local").unwrap();
        let unknown = TaskId(500);
        let snapshot = vec![
            snapshot_task(local, NOW - 1, TaskChange::Title(title("stale"))).apply(
                &TaskChange::Status(TaskStatus::Done),
                Hlc::new(NOW + 1, 0, OTHER),
            ),
            snapshot_task(unknown, NOW, TaskChange::Title(title("remote"))),
        ];

        replica.apply_snapshot(&snapshot, Some(Seq(10))).unwrap();

        let task = replica.task(local).unwrap();
        assert_eq!(task.title(), Some(&title("local")));
        assert_eq!(task.status(), TaskStatus::Done);
        assert_eq!(
            replica.task(unknown).unwrap().title(),
            Some(&title("remote"))
        );
        assert_eq!(replica.last_seq(), Some(Seq(10)));
    }

    #[test]
    fn apply_snapshot_keeps_pending_ops() {
        let mut replica = replica();
        replica.create("a").unwrap();
        let pending = replica.pending_ops().to_vec();

        replica.apply_snapshot(&[], Some(Seq(10))).unwrap();

        assert_eq!(replica.pending_ops(), pending.as_slice());
    }

    #[test]
    fn apply_snapshot_never_moves_cursor_backwards() {
        let mut replica = replica();
        replica.apply_snapshot(&[], Some(Seq(10))).unwrap();

        replica.apply_snapshot(&[], Some(Seq(4))).unwrap();
        replica.apply_snapshot(&[], None).unwrap();

        assert_eq!(replica.last_seq(), Some(Seq(10)));
    }

    #[test]
    fn local_writes_after_snapshot_are_newer_than_its_contents() {
        let mut replica = replica();
        let id = TaskId(500);
        let future = NOW + 60_000;
        replica
            .apply_snapshot(
                &[snapshot_task(
                    id,
                    future,
                    TaskChange::Title(title("remote")),
                )],
                Some(Seq(1)),
            )
            .unwrap();

        replica.rename(id, "local").unwrap();

        assert_eq!(replica.task(id).unwrap().title(), Some(&title("local")));
    }

    #[test]
    fn apply_snapshot_clock_overflow_changes_nothing() {
        let mut replica = replica();
        let id = TaskId(500);
        let task = Task::empty(id).apply(
            &TaskChange::Title(title("a")),
            Hlc::new(NOW, u32::MAX, OTHER),
        );

        let result = replica.apply_snapshot(&[task], Some(Seq(1)));

        assert_eq!(result, Err(RemoteError::Clock(HlcError::CounterOverflow)));
        assert!(replica.task(id).is_none());
        assert_eq!(replica.last_seq(), None);
    }

    #[test]
    fn acknowledge_removes_only_listed_ops() {
        let mut replica = replica();
        replica.create("a").unwrap();
        let ops = replica.pending_ops().to_vec();

        replica.acknowledge(&[ops[1].op_id]);

        assert_eq!(replica.pending_ops(), &ops[..1]);
    }

    #[test]
    fn acknowledge_ignores_unknown_ids() {
        let mut replica = replica();
        replica.create("a").unwrap();
        let ops = replica.pending_ops().to_vec();

        replica.acknowledge(&[OpId(u128::MAX)]);

        assert_eq!(replica.pending_ops(), ops.as_slice());
    }

    #[test]
    fn op_acknowledged_after_snapshot_skipped_its_echo() {
        let mut replica = replica();
        let id = replica.create("a").unwrap();
        let sent: Vec<OpId> = replica.pending_ops().iter().map(|op| op.op_id).collect();
        let snapshot = vec![replica.task(id).unwrap().clone()];
        replica.apply_snapshot(&snapshot, Some(Seq(2))).unwrap();

        replica.acknowledge(&sent);

        assert!(replica.pending_ops().is_empty());
    }
}
