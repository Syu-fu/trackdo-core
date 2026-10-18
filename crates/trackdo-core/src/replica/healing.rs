//! Writing back timer stops that syncing left only derived.

use super::Replica;
use super::time_commands::entry_target;
use crate::env::{Clock, IdGen};
use crate::hlc::HlcError;
use crate::time_entry::TimeEntryChange;
use crate::tracking::derived_stops;

impl<C: Clock, G: IdGen> Replica<C, G> {
    /// Emits a stop for every entry that is running in storage but derived
    /// to have stopped, so stored state catches up with what is shown.
    ///
    /// Every replica derives the same stop from the same state, so replicas
    /// that heal the same entry concurrently write the same value.
    pub(super) fn heal(&mut self) -> Result<(), HlcError> {
        let stops = derived_stops(&self.tasks, &self.entries)
            .into_iter()
            .filter_map(|(id, stop)| {
                let entry = self.entries.get(&id)?;
                let change = TimeEntryChange::StoppedAt(Some(stop));
                Some(entry_target(id, entry.task_id(), change))
            })
            .collect();
        self.emit(stops)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::super::{RemoteError, Snapshot};
    use crate::hlc::Hlc;
    use crate::op::{Op, OpId, OpTarget, Seq};
    use crate::task::{Task, TaskChange, TaskId};
    use crate::time_entry::{TimeEntryChange, TimeEntryId, Timestamp};

    fn ms(millis: u64) -> Timestamp {
        Timestamp(millis)
    }

    /// Delivers `changes` to `entry` as separate server events, as another
    /// device would have sent them.
    fn deliver(
        replica: &mut TestReplica,
        entry: TimeEntryId,
        task: TaskId,
        wall: u64,
        changes: &[TimeEntryChange],
    ) {
        for (counter, change) in changes.iter().enumerate() {
            let seq = replica.last_seq().map_or(1, |Seq(seq)| seq + 1);
            let op = Op::time_entry(
                OpId(entry.0 * 100 + counter as u128),
                entry,
                task,
                Hlc::new(wall, counter as u32, OTHER),
                change.clone(),
            );
            replica.apply_remote(&op, Seq(seq)).unwrap();
        }
    }

    fn remote_start(replica: &mut TestReplica, entry: TimeEntryId, task: TaskId, at: u64) {
        deliver(
            replica,
            entry,
            task,
            at,
            &[
                TimeEntryChange::StartedAt(ms(at)),
                TimeEntryChange::StoppedAt(None),
            ],
        );
    }

    fn stops_written(replica: &TestReplica) -> Vec<(TimeEntryId, Timestamp)> {
        replica
            .pending_ops()
            .iter()
            .filter_map(|op| match op.target {
                OpTarget::TimeEntry {
                    entry_id,
                    change: TimeEntryChange::StoppedAt(Some(stop)),
                    ..
                } => Some((entry_id, stop)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn later_remote_start_stops_local_timer_in_storage() {
        let mut replica = replica();
        let task = replica.create("local").unwrap();
        let local = replica.start(task).unwrap();

        remote_start(&mut replica, TimeEntryId(500), TaskId(77), NOW + 10);

        assert_eq!(stops_written(&replica), vec![(local, ms(NOW + 10))]);
        assert_eq!(
            replica.time_entry(local).unwrap().stopped_at(),
            Some(ms(NOW + 10))
        );
        assert!(!replica.entries_for(task)[0].is_stop_derived());
    }

    #[test]
    fn earlier_remote_start_is_stopped_by_this_replica() {
        let mut replica = replica();
        let task = replica.create("local").unwrap();
        replica.start(task).unwrap();
        let remote = TimeEntryId(500);

        remote_start(&mut replica, remote, TaskId(77), NOW - 10);

        assert_eq!(stops_written(&replica), vec![(remote, ms(NOW))]);
        let op = replica.pending_ops().last().unwrap();
        assert!(matches!(
            op.target,
            OpTarget::TimeEntry { task_id, .. } if task_id == TaskId(77)
        ));
    }

    #[test]
    fn partially_received_entry_does_not_stop_local_timer() {
        let mut replica = replica();
        let task = replica.create("local").unwrap();
        replica.start(task).unwrap();

        deliver(
            &mut replica,
            TimeEntryId(500),
            TaskId(77),
            NOW + 10,
            &[TimeEntryChange::StartedAt(ms(NOW + 10))],
        );

        assert_eq!(stops_written(&replica), vec![]);
        assert!(replica.is_in_progress(task));
    }

    #[test]
    fn remote_stopped_entry_causes_no_writes() {
        let mut replica = replica();
        let task = replica.create("local").unwrap();
        replica.start(task).unwrap();
        let pending = replica.pending_ops().len();

        deliver(
            &mut replica,
            TimeEntryId(500),
            TaskId(77),
            NOW + 10,
            &[
                TimeEntryChange::StartedAt(ms(NOW - 500)),
                TimeEntryChange::StoppedAt(Some(ms(NOW - 100))),
            ],
        );

        assert_eq!(replica.pending_ops().len(), pending);
    }

    #[test]
    fn remote_task_deletion_stops_its_timer_at_deletion_time() {
        let mut replica = replica();
        let task = replica.create("work").unwrap();
        let id = replica.start(task).unwrap();
        let delete = Op::task(
            OpId(900),
            task,
            Hlc::new(NOW + 30, 0, OTHER),
            TaskChange::Deleted(true),
        );

        replica.apply_remote(&delete, Seq(1)).unwrap();

        assert_eq!(stops_written(&replica), vec![(id, ms(NOW + 30))]);
    }

    #[test]
    fn snapshot_deleting_task_stops_its_timer() {
        let mut replica = replica();
        let task = replica.create("work").unwrap();
        let id = replica.start(task).unwrap();
        let deleted =
            Task::empty(task).apply(&TaskChange::Deleted(true), Hlc::new(NOW + 30, 0, OTHER));

        replica
            .apply_snapshot(&Snapshot {
                tasks: vec![deleted],
                time_entries: Vec::new(),
                seq: Some(Seq(5)),
            })
            .unwrap();

        assert_eq!(stops_written(&replica), vec![(id, ms(NOW + 30))]);
    }

    #[test]
    fn healing_happens_once() {
        let mut replica = replica();
        let task = replica.create("local").unwrap();
        replica.start(task).unwrap();
        remote_start(&mut replica, TimeEntryId(500), TaskId(77), NOW + 10);
        let pending = replica.pending_ops().len();

        let unrelated = Op::task(
            OpId(901),
            TaskId(78),
            Hlc::new(NOW + 20, 0, OTHER),
            TaskChange::Deleted(false),
        );
        replica.apply_remote(&unrelated, Seq(100)).unwrap();

        assert_eq!(replica.pending_ops().len(), pending);
    }

    #[test]
    fn heal_clock_failure_is_reported_after_applying_the_op() {
        let mut replica = replica();
        let task = replica.create("local").unwrap();
        replica.start(task).unwrap();
        let entry = TimeEntryId(500);
        let start = Op::time_entry(
            OpId(1),
            entry,
            TaskId(77),
            Hlc::new(NOW + 10, 0, OTHER),
            TimeEntryChange::StartedAt(ms(NOW + 10)),
        );
        replica.apply_remote(&start, Seq(1)).unwrap();
        let running = Op::time_entry(
            OpId(2),
            entry,
            TaskId(77),
            Hlc::new(NOW + 10, u32::MAX - 1, OTHER),
            TimeEntryChange::StoppedAt(None),
        );

        let result = replica.apply_remote(&running, Seq(2));

        assert_eq!(
            result,
            Err(RemoteError::Clock(crate::hlc::HlcError::CounterOverflow))
        );
        assert!(replica.time_entry(entry).unwrap().is_running());
        assert_eq!(replica.last_seq(), Some(Seq(2)));
    }
}
