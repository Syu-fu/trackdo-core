//! Commands on time entries.

use super::{CommandError, Replica};
use crate::env::{Clock, IdGen};
use crate::op::OpTarget;
use crate::task::{TaskChange, TaskId, TaskStatus};
use crate::time_entry::{TimeEntry, TimeEntryChange, TimeEntryId, Timestamp};

impl<C: Clock, G: IdGen> Replica<C, G> {
    /// Starts a timer on `task_id`, stopping any running timer at the same
    /// instant so that at most one runs at a time. Working on a done task
    /// reopens it.
    pub fn start(&mut self, task_id: TaskId) -> Result<TimeEntryId, CommandError> {
        self.ensure_editable(task_id)?;
        let now = self.now();
        let stops = self.stop_running(now, |_| true);
        let id = self.ids.time_entry_id();
        let starts = [
            TimeEntryChange::StartedAt(now),
            TimeEntryChange::StoppedAt(None),
        ]
        .map(|change| entry_target(id, task_id, change));
        let reopen = self
            .tasks
            .get(&task_id)
            .is_some_and(|task| task.status() == TaskStatus::Done)
            .then_some(OpTarget::Task {
                task_id,
                change: TaskChange::Status(TaskStatus::Todo),
            });
        self.emit::<CommandError>(stops.into_iter().chain(starts).chain(reopen).collect())?;
        Ok(id)
    }

    pub fn stop(&mut self, entry_id: TimeEntryId) -> Result<(), CommandError> {
        let entry = self.editable_entry(entry_id)?;
        if !entry.is_running() {
            return Err(CommandError::NotRunning(entry_id));
        }
        let stop = stop_target(&entry, self.now());
        self.emit(vec![stop])
    }

    /// Corrects the interval of an entry. `stopped_at` is `None` only for a
    /// running entry that should keep running.
    ///
    /// Only fields that actually change are written, so a concurrent edit of
    /// the other field on another device is kept.
    pub fn edit_times(
        &mut self,
        entry_id: TimeEntryId,
        started_at: Timestamp,
        stopped_at: Option<Timestamp>,
    ) -> Result<(), CommandError> {
        let entry = self.editable_entry(entry_id)?;
        self.validate_interval(started_at, stopped_at)?;
        if stopped_at.is_none() && !entry.is_running() {
            return Err(CommandError::NotRunning(entry_id));
        }

        let started = (entry.started_at() != Some(started_at))
            .then_some(TimeEntryChange::StartedAt(started_at));
        let stopped =
            (entry.stopped_at() != stopped_at).then_some(TimeEntryChange::StoppedAt(stopped_at));
        let targets = started
            .into_iter()
            .chain(stopped)
            .map(|change| entry_target(entry_id, entry.task_id(), change))
            .collect();
        self.emit(targets)
    }

    pub fn delete_entry(&mut self, entry_id: TimeEntryId) -> Result<(), CommandError> {
        let entry = self.editable_entry(entry_id)?;
        let delete = entry_target(entry_id, entry.task_id(), TimeEntryChange::Deleted(true));
        self.emit(vec![delete])
    }

    /// Records past work. May overlap other entries.
    pub fn add_entry(
        &mut self,
        task_id: TaskId,
        started_at: Timestamp,
        stopped_at: Timestamp,
    ) -> Result<TimeEntryId, CommandError> {
        self.ensure_editable(task_id)?;
        self.validate_interval(started_at, Some(stopped_at))?;
        let id = self.ids.time_entry_id();
        let targets = [
            TimeEntryChange::StartedAt(started_at),
            TimeEntryChange::StoppedAt(Some(stopped_at)),
        ]
        .map(|change| entry_target(id, task_id, change));
        self.emit::<CommandError>(targets.into())?;
        Ok(id)
    }

    pub(super) fn now(&self) -> Timestamp {
        Timestamp(self.clock.now_millis())
    }

    /// Operations stopping every running entry that `matches`.
    pub(super) fn stop_running(
        &self,
        now: Timestamp,
        matches: impl Fn(&TimeEntry) -> bool,
    ) -> Vec<OpTarget> {
        self.entries
            .values()
            .filter(|entry| entry.is_running() && !entry.is_deleted() && matches(entry))
            .map(|entry| stop_target(entry, now))
            .collect()
    }

    fn editable_entry(&self, id: TimeEntryId) -> Result<TimeEntry, CommandError> {
        let entry = self
            .entries
            .get(&id)
            .ok_or(CommandError::EntryNotFound(id))?;
        if entry.is_deleted() {
            return Err(CommandError::EntryDeleted(id));
        }
        self.ensure_editable(entry.task_id())?;
        Ok(entry.clone())
    }

    /// User-entered times must form a non-negative interval in the past.
    fn validate_interval(
        &self,
        started: Timestamp,
        stopped: Option<Timestamp>,
    ) -> Result<(), CommandError> {
        let now = self.now();
        if let Some(future) = [Some(started), stopped]
            .into_iter()
            .flatten()
            .find(|t| *t > now)
        {
            return Err(CommandError::InFuture(future));
        }
        match stopped {
            Some(stopped) if stopped < started => {
                Err(CommandError::StopBeforeStart { started, stopped })
            }
            _ => Ok(()),
        }
    }
}

pub(super) const fn entry_target(
    entry_id: TimeEntryId,
    task_id: TaskId,
    change: TimeEntryChange,
) -> OpTarget {
    OpTarget::TimeEntry {
        entry_id,
        task_id,
        change,
    }
}

/// Stops `entry` at `now`, or at its start if the clock went backwards, so
/// that stopping never produces an inverted interval.
fn stop_target(entry: &TimeEntry, now: Timestamp) -> OpTarget {
    let stopped = entry.started_at().map_or(now, |started| started.max(now));
    entry_target(
        entry.id(),
        entry.task_id(),
        TimeEntryChange::StoppedAt(Some(stopped)),
    )
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::super::{RemoteError, ReplicaState};
    use super::*;
    use crate::hlc::Hlc;
    use crate::op::{Op, OpId, OpTarget, Seq};
    use crate::time_entry::TimeEntryChange;

    fn ms(millis: u64) -> Timestamp {
        Timestamp(millis)
    }

    fn entry_changes(replica: &TestReplica) -> Vec<(TimeEntryId, TimeEntryChange)> {
        replica
            .pending_ops()
            .iter()
            .filter_map(|op| match &op.target {
                OpTarget::TimeEntry {
                    entry_id, change, ..
                } => Some((*entry_id, change.clone())),
                OpTarget::Task { .. } => None,
            })
            .collect()
    }

    fn with_task() -> (TestReplica, TaskId) {
        let mut replica = replica();
        let task = replica.create("work").unwrap();
        (replica, task)
    }

    fn running(replica: &TestReplica) -> Vec<TimeEntryId> {
        replica
            .time_entries()
            .filter(|entry| entry.is_running())
            .map(|entry| entry.id())
            .collect()
    }

    #[test]
    fn start_creates_running_entry_for_task() {
        let (mut replica, task) = with_task();

        let id = replica.start(task).unwrap();

        let entry = replica.time_entry(id).unwrap();
        assert_eq!(entry.task_id(), task);
        assert_eq!(entry.started_at(), Some(ms(NOW)));
        assert!(entry.is_running());
        assert_eq!(
            entry_changes(&replica),
            vec![
                (id, TimeEntryChange::StartedAt(ms(NOW))),
                (id, TimeEntryChange::StoppedAt(None)),
            ]
        );
    }

    #[test]
    fn ops_carry_the_entry_task() {
        let (mut replica, task) = with_task();

        replica.start(task).unwrap();

        assert!(replica.pending_ops().iter().all(|op| match op.target {
            OpTarget::Task { task_id, .. } | OpTarget::TimeEntry { task_id, .. } => task_id == task,
        }));
    }

    #[test]
    fn start_stops_running_entry_at_the_same_instant() {
        let (mut replica, task) = with_task();
        let other = replica.create("other").unwrap();
        let first = replica.start(task).unwrap();
        set_now(&mut replica, NOW + 5_000);

        let second = replica.start(other).unwrap();

        assert_eq!(
            replica.time_entry(first).unwrap().stopped_at(),
            Some(ms(NOW + 5_000))
        );
        assert_eq!(
            replica.time_entry(second).unwrap().started_at(),
            Some(ms(NOW + 5_000))
        );
        assert_eq!(running(&replica), vec![second]);
    }

    #[test]
    fn start_never_stops_running_entry_before_its_start() {
        let (mut replica, task) = with_task();
        set_now(&mut replica, NOW + 5_000);
        let first = replica.start(task).unwrap();
        set_now(&mut replica, NOW + 3_000);

        replica.start(task).unwrap();

        let first = replica.time_entry(first).unwrap();
        assert_eq!(first.stopped_at(), Some(ms(NOW + 5_000)));
        assert!(!first.needs_attention());
    }

    #[test]
    fn start_on_unknown_or_deleted_task_fails() {
        let (mut replica, task) = with_task();
        replica.delete(task).unwrap();
        let pending = replica.pending_ops().len();

        assert_eq!(
            replica.start(TaskId(999)),
            Err(CommandError::NotFound(TaskId(999)))
        );
        assert_eq!(replica.start(task), Err(CommandError::Deleted(task)));
        assert_eq!(replica.pending_ops().len(), pending);
        assert_eq!(replica.time_entries().count(), 0);
    }

    #[test]
    fn stop_stops_running_entry_now() {
        let (mut replica, task) = with_task();
        let id = replica.start(task).unwrap();
        set_now(&mut replica, NOW + 2_000);

        replica.stop(id).unwrap();

        let entry = replica.time_entry(id).unwrap();
        assert_eq!(entry.stopped_at(), Some(ms(NOW + 2_000)));
        assert_eq!(
            entry_changes(&replica).last(),
            Some(&(id, TimeEntryChange::StoppedAt(Some(ms(NOW + 2_000)))))
        );
    }

    #[test]
    fn stop_never_precedes_start() {
        let (mut replica, task) = with_task();
        set_now(&mut replica, NOW + 5_000);
        let id = replica.start(task).unwrap();
        set_now(&mut replica, NOW);

        replica.stop(id).unwrap();

        assert_eq!(
            replica.time_entry(id).unwrap().stopped_at(),
            Some(ms(NOW + 5_000))
        );
    }

    #[test]
    fn stop_fails_unless_entry_is_running() {
        let (mut replica, task) = with_task();
        let id = replica.add_entry(task, ms(100), ms(200)).unwrap();

        assert_eq!(replica.stop(id), Err(CommandError::NotRunning(id)));
        assert_eq!(
            replica.stop(TimeEntryId(999)),
            Err(CommandError::EntryNotFound(TimeEntryId(999)))
        );
    }

    #[test]
    fn add_entry_records_stopped_interval_without_stopping_timer() {
        let (mut replica, task) = with_task();
        let timer = replica.start(task).unwrap();

        let id = replica.add_entry(task, ms(100), ms(400)).unwrap();

        let entry = replica.time_entry(id).unwrap();
        assert_eq!(entry.started_at(), Some(ms(100)));
        assert_eq!(entry.stopped_at(), Some(ms(400)));
        assert_eq!(running(&replica), vec![timer]);
    }

    #[test]
    fn add_entry_allows_zero_length() {
        let (mut replica, task) = with_task();

        let id = replica.add_entry(task, ms(100), ms(100)).unwrap();

        assert!(!replica.time_entry(id).unwrap().is_running());
    }

    #[test]
    fn add_entry_validates_interval_and_task() {
        let (mut replica, task) = with_task();
        let pending = replica.pending_ops().len();

        assert_eq!(
            replica.add_entry(task, ms(300), ms(200)),
            Err(CommandError::StopBeforeStart {
                started: ms(300),
                stopped: ms(200)
            })
        );
        assert_eq!(
            replica.add_entry(task, ms(100), ms(NOW + 1)),
            Err(CommandError::InFuture(ms(NOW + 1)))
        );
        assert_eq!(
            replica.add_entry(TaskId(999), ms(100), ms(200)),
            Err(CommandError::NotFound(TaskId(999)))
        );
        assert_eq!(replica.pending_ops().len(), pending);
    }

    #[test]
    fn edit_times_writes_only_changed_fields() {
        let (mut replica, task) = with_task();
        let id = replica.add_entry(task, ms(100), ms(500)).unwrap();
        let before = entry_changes(&replica).len();

        replica.edit_times(id, ms(200), Some(ms(500))).unwrap();

        assert_eq!(replica.time_entry(id).unwrap().started_at(), Some(ms(200)));
        assert_eq!(
            entry_changes(&replica)[before..],
            [(id, TimeEntryChange::StartedAt(ms(200)))]
        );
    }

    #[test]
    fn edit_times_without_changes_emits_nothing() {
        let (mut replica, task) = with_task();
        let id = replica.add_entry(task, ms(100), ms(500)).unwrap();
        let pending = replica.pending_ops().len();

        replica.edit_times(id, ms(100), Some(ms(500))).unwrap();

        assert_eq!(replica.pending_ops().len(), pending);
    }

    #[test]
    fn edit_times_keeps_running_entry_running() {
        let (mut replica, task) = with_task();
        let id = replica.start(task).unwrap();

        replica.edit_times(id, ms(NOW - 600), None).unwrap();

        let entry = replica.time_entry(id).unwrap();
        assert_eq!(entry.started_at(), Some(ms(NOW - 600)));
        assert!(entry.is_running());
    }

    #[test]
    fn edit_times_may_stop_running_entry() {
        let (mut replica, task) = with_task();
        let id = replica.start(task).unwrap();

        replica.edit_times(id, ms(NOW), Some(ms(NOW))).unwrap();

        assert!(!replica.time_entry(id).unwrap().is_running());
    }

    #[test]
    fn edit_times_cannot_resume_stopped_entry() {
        let (mut replica, task) = with_task();
        let id = replica.add_entry(task, ms(100), ms(500)).unwrap();

        assert_eq!(
            replica.edit_times(id, ms(100), None),
            Err(CommandError::NotRunning(id))
        );
    }

    #[test]
    fn edit_times_validates_interval() {
        let (mut replica, task) = with_task();
        let id = replica.add_entry(task, ms(100), ms(500)).unwrap();
        let pending = replica.pending_ops().len();

        assert_eq!(
            replica.edit_times(id, ms(600), Some(ms(500))),
            Err(CommandError::StopBeforeStart {
                started: ms(600),
                stopped: ms(500)
            })
        );
        assert_eq!(
            replica.edit_times(id, ms(NOW + 1), None),
            Err(CommandError::InFuture(ms(NOW + 1)))
        );
        assert_eq!(replica.pending_ops().len(), pending);
    }

    #[test]
    fn delete_entry_marks_it_deleted() {
        let (mut replica, task) = with_task();
        let id = replica.add_entry(task, ms(100), ms(500)).unwrap();

        replica.delete_entry(id).unwrap();

        assert!(replica.time_entry(id).unwrap().is_deleted());
        assert_eq!(
            entry_changes(&replica).last(),
            Some(&(id, TimeEntryChange::Deleted(true)))
        );
    }

    #[test]
    fn commands_on_deleted_entry_fail() {
        let (mut replica, task) = with_task();
        let id = replica.start(task).unwrap();
        replica.delete_entry(id).unwrap();
        let pending = replica.pending_ops().len();

        assert_eq!(replica.stop(id), Err(CommandError::EntryDeleted(id)));
        assert_eq!(
            replica.edit_times(id, ms(100), None),
            Err(CommandError::EntryDeleted(id))
        );
        assert_eq!(
            replica.delete_entry(id),
            Err(CommandError::EntryDeleted(id))
        );
        assert_eq!(replica.pending_ops().len(), pending);
    }

    #[test]
    fn commands_on_entry_of_deleted_task_fail() {
        let (mut replica, task) = with_task();
        let id = replica.add_entry(task, ms(100), ms(500)).unwrap();
        replica.delete(task).unwrap();

        assert_eq!(
            replica.edit_times(id, ms(200), Some(ms(500))),
            Err(CommandError::Deleted(task))
        );
        assert_eq!(replica.delete_entry(id), Err(CommandError::Deleted(task)));
    }

    fn remote_entry_op(op_id: u128, entry: TimeEntryId, task: TaskId) -> Op {
        Op::time_entry(
            OpId(op_id),
            entry,
            task,
            Hlc::new(NOW, 0, OTHER),
            TimeEntryChange::StartedAt(ms(100)),
        )
    }

    #[test]
    fn apply_remote_creates_unknown_entry() {
        let mut replica = replica();
        let entry = TimeEntryId(500);

        replica
            .apply_remote(&remote_entry_op(1, entry, TaskId(9)), Seq(1))
            .unwrap();

        let entry = replica.time_entry(entry).unwrap();
        assert_eq!(entry.task_id(), TaskId(9));
        assert_eq!(entry.started_at(), Some(ms(100)));
    }

    #[test]
    fn apply_remote_rejects_entry_moved_to_another_task() {
        let (mut replica, task) = with_task();
        let id = replica.add_entry(task, ms(100), ms(500)).unwrap();
        let before = replica.state();

        let result = replica.apply_remote(&remote_entry_op(900, id, TaskId(999)), Seq(1));

        assert_eq!(
            result,
            Err(RemoteError::TaskMismatch {
                entry: id,
                expected: task,
                actual: TaskId(999),
            })
        );
        assert_eq!(replica.state(), before);
    }

    #[test]
    fn state_roundtrip_keeps_entries() {
        let (mut replica, task) = with_task();
        replica.start(task).unwrap();
        let state = replica.state();

        let restored = Replica::restore(state.clone(), FixedClock(NOW), SequentialIds(1_000));

        assert_eq!(restored.state(), state);
    }

    #[test]
    fn restore_merges_duplicate_entries() {
        let (mut replica, task) = with_task();
        let id = replica.start(task).unwrap();
        let running = replica.state();
        replica.stop(id).unwrap();
        let stopped = replica.state();
        let state = ReplicaState {
            time_entries: [running.time_entries, stopped.time_entries.clone()].concat(),
            ..stopped.clone()
        };

        let restored = Replica::restore(state, FixedClock(NOW), SequentialIds(1_000));

        assert_eq!(restored.state(), stopped);
    }

    #[test]
    fn complete_stops_running_timer_of_the_task() {
        let (mut replica, task) = with_task();
        let id = replica.start(task).unwrap();
        set_now(&mut replica, NOW + 1_000);

        replica.complete(task).unwrap();

        assert_eq!(
            replica.time_entry(id).unwrap().stopped_at(),
            Some(ms(NOW + 1_000))
        );
        assert_eq!(replica.task(task).unwrap().status(), TaskStatus::Done);
    }

    #[test]
    fn complete_leaves_timer_of_another_task_running() {
        let (mut replica, task) = with_task();
        let other = replica.create("other").unwrap();
        let id = replica.start(other).unwrap();

        replica.complete(task).unwrap();

        assert_eq!(running(&replica), vec![id]);
    }

    #[test]
    fn delete_stops_running_timer_of_the_task() {
        let (mut replica, task) = with_task();
        let id = replica.start(task).unwrap();
        set_now(&mut replica, NOW + 1_000);

        replica.delete(task).unwrap();

        assert_eq!(
            replica.time_entry(id).unwrap().stopped_at(),
            Some(ms(NOW + 1_000))
        );
        assert!(replica.task(task).unwrap().is_deleted());
    }

    #[test]
    fn start_on_done_task_reopens_it() {
        let (mut replica, task) = with_task();
        replica.complete(task).unwrap();

        replica.start(task).unwrap();

        assert_eq!(replica.task(task).unwrap().status(), TaskStatus::Todo);
        assert_eq!(
            changes(&replica).last(),
            Some(&TaskChange::Status(TaskStatus::Todo))
        );
    }

    #[test]
    fn start_on_todo_task_leaves_status_untouched() {
        let (mut replica, task) = with_task();
        let before = changes(&replica);

        replica.start(task).unwrap();

        assert_eq!(changes(&replica), before);
    }
}
