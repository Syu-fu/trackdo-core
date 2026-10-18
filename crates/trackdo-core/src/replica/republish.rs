//! Sending writes again when the server's log was replaced.

use std::collections::BTreeMap;

use super::{Replica, Snapshot};
use crate::env::{Clock, IdGen};
use crate::hlc::Hlc;
use crate::op::{Op, OpTarget};
use crate::task::Task;
use crate::time_entry::TimeEntry;

impl<C: Clock, G: IdGen> Replica<C, G> {
    /// Queues every write this replica holds that `snapshot`, of a replaced
    /// log, lacks: writes acknowledged by the old log may be lost with it.
    ///
    /// Writes keep their original timestamps, so they never override what
    /// was written after them, and replicas sending the same write again
    /// change nothing. Writes still pending are sent anyway.
    pub(super) fn republish(&mut self, snapshot: &Snapshot) {
        let missing: Vec<(OpTarget, Hlc)> = self
            .missing_task_writes(&snapshot.tasks)
            .chain(self.missing_entry_writes(&snapshot.time_entries))
            .filter(|(target, hlc)| {
                !self
                    .pending
                    .iter()
                    .any(|op| op.hlc == *hlc && op.target == *target)
            })
            .collect();
        for (target, hlc) in missing {
            let op_id = self.ids.op_id();
            self.pending.push(Op { op_id, hlc, target });
        }
    }

    fn missing_task_writes(&self, snapshot: &[Task]) -> impl Iterator<Item = (OpTarget, Hlc)> {
        let known: BTreeMap<_, _> = snapshot.iter().map(|task| (task.id(), task)).collect();
        self.tasks.values().flat_map(move |task| {
            let held = known
                .get(&task.id())
                .map_or_else(|| Task::empty(task.id()), |&task| task.clone());
            task.writes()
                .filter(move |(change, hlc)| held.apply(change, *hlc) != held)
                .map(|(change, hlc)| {
                    let task_id = task.id();
                    (OpTarget::Task { task_id, change }, hlc)
                })
        })
    }

    fn missing_entry_writes(
        &self,
        snapshot: &[TimeEntry],
    ) -> impl Iterator<Item = (OpTarget, Hlc)> {
        let known: BTreeMap<_, _> = snapshot.iter().map(|entry| (entry.id(), entry)).collect();
        self.entries.values().flat_map(move |entry| {
            let held = known.get(&entry.id()).map_or_else(
                || TimeEntry::empty(entry.id(), entry.task_id()),
                |&entry| entry.clone(),
            );
            entry
                .writes()
                .filter(move |(change, hlc)| held.apply(change, *hlc) != held)
                .map(|(change, hlc)| {
                    let target = OpTarget::TimeEntry {
                        entry_id: entry.id(),
                        task_id: entry.task_id(),
                        change,
                    };
                    (target, hlc)
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::Snapshot;
    use super::super::test_support::*;
    use crate::hlc::Hlc;
    use crate::op::{LogId, Op, OpId, OpTarget, Seq};
    use crate::task::{Task, TaskChange, TaskId, TaskStatus};
    use crate::time_entry::{TimeEntry, TimeEntryChange, TimeEntryId, Timestamp};

    const RESTORED: LogId = LogId(2);
    const TASK: TaskId = TaskId(500);

    fn snapshot(log_id: LogId, tasks: Vec<Task>, time_entries: Vec<TimeEntry>) -> Snapshot {
        Snapshot {
            log_id,
            tasks,
            time_entries,
            seq: None,
        }
    }

    fn task_op(op_id: u128, wall: u64, change: TaskChange) -> Op {
        Op::task(OpId(op_id), TASK, Hlc::new(wall, 0, OTHER), change)
    }

    /// A replica on `LOG` that received `ops` from the server and has
    /// nothing pending.
    fn synced(ops: &[Op]) -> TestReplica {
        let mut replica = replica();
        replica
            .apply_snapshot(&snapshot(LOG, Vec::new(), Vec::new()))
            .unwrap();
        for (seq, op) in ops.iter().enumerate() {
            replica.apply_remote(op, Seq(seq as u64 + 1)).unwrap();
        }
        replica
    }

    fn writes(replica: &TestReplica) -> Vec<(OpTarget, Hlc)> {
        replica
            .pending_ops()
            .iter()
            .map(|op| (op.target.clone(), op.hlc))
            .collect()
    }

    #[test]
    fn republishes_writes_a_replaced_log_lacks() {
        let renamed = task_op(900, NOW - 10, TaskChange::Title(title("milk")));
        let mut replica = synced(&[renamed.clone()]);

        replica
            .apply_snapshot(&snapshot(RESTORED, Vec::new(), Vec::new()))
            .unwrap();

        assert_eq!(writes(&replica), vec![(renamed.target, renamed.hlc)]);
        assert_ne!(replica.pending_ops()[0].op_id, renamed.op_id);
    }

    #[test]
    fn republishes_only_registers_newer_than_the_snapshot() {
        let renamed = task_op(900, NOW - 10, TaskChange::Title(title("milk")));
        let done = task_op(901, NOW - 10, TaskChange::Status(TaskStatus::Done));
        let mut replica = synced(&[renamed, done.clone()]);
        let restored = Task::empty(TASK)
            .apply(
                &TaskChange::Title(title("eggs")),
                Hlc::new(NOW - 5, 0, OTHER),
            )
            .apply(
                &TaskChange::Status(TaskStatus::Todo),
                Hlc::new(NOW - 20, 0, OTHER),
            );

        replica
            .apply_snapshot(&snapshot(RESTORED, vec![restored], Vec::new()))
            .unwrap();

        assert_eq!(writes(&replica), vec![(done.target, done.hlc)]);
        assert_eq!(replica.task(TASK).unwrap().title(), Some(&title("eggs")));
    }

    #[test]
    fn republishes_time_entries() {
        let entry = TimeEntryId(600);
        let started = Op::time_entry(
            OpId(900),
            entry,
            TASK,
            Hlc::new(NOW - 10, 0, OTHER),
            TimeEntryChange::StartedAt(Timestamp(NOW - 100)),
        );
        let stopped = Op::time_entry(
            OpId(901),
            entry,
            TASK,
            Hlc::new(NOW - 10, 1, OTHER),
            TimeEntryChange::StoppedAt(Some(Timestamp(NOW - 50))),
        );
        let mut replica = synced(&[started.clone(), stopped.clone()]);

        replica
            .apply_snapshot(&snapshot(RESTORED, Vec::new(), Vec::new()))
            .unwrap();

        assert_eq!(
            writes(&replica),
            vec![(started.target, started.hlc), (stopped.target, stopped.hlc)]
        );
    }

    #[test]
    fn pending_writes_are_not_republished() {
        let mut replica = replica();
        replica.create("milk").unwrap();
        let pending = replica.pending_ops().to_vec();

        replica
            .apply_snapshot(&snapshot(RESTORED, Vec::new(), Vec::new()))
            .unwrap();

        assert_eq!(replica.pending_ops(), pending.as_slice());
    }

    #[test]
    fn snapshot_of_the_same_log_republishes_nothing() {
        let renamed = task_op(900, NOW - 10, TaskChange::Title(title("milk")));
        let mut replica = synced(&[renamed]);

        replica
            .apply_snapshot(&snapshot(LOG, Vec::new(), Vec::new()))
            .unwrap();

        assert!(replica.pending_ops().is_empty());
    }
}
