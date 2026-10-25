//! Moving tasks within the task tree.

use super::{CommandError, Replica};
use crate::env::{Clock, IdGen};
use crate::task::{TaskChange, TaskId};

impl<C: Clock, G: IdGen> Replica<C, G> {
    /// Moves `id` under `parent`, or to the top level for `None`.
    pub fn set_parent(&mut self, id: TaskId, parent: Option<TaskId>) -> Result<(), CommandError> {
        self.ensure_editable(id)?;
        if let Some(parent) = parent {
            self.ensure_editable(parent)?;
            if self.hierarchy().is_within(parent, id) {
                return Err(CommandError::Cycle { task: id, parent });
            }
        }
        self.emit_task(id, vec![TaskChange::Parent(parent)])
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use crate::hlc::Hlc;
    use crate::op::{Op, OpId, OpTarget, Seq};
    use crate::time_entry::{TimeEntryChange, Timestamp};

    /// Root, child and grandchild, each under the previous one.
    fn family(replica: &mut TestReplica) -> [TaskId; 3] {
        let root = replica.create("root").unwrap();
        let child = replica.create("child").unwrap();
        let grandchild = replica.create("grandchild").unwrap();
        replica.set_parent(child, Some(root)).unwrap();
        replica.set_parent(grandchild, Some(child)).unwrap();
        [root, child, grandchild]
    }

    #[test]
    fn set_parent_moves_task_under_parent() {
        let mut replica = replica();
        let [root, child, grandchild] = family(&mut replica);

        let tree = replica.hierarchy();
        assert_eq!(tree.parent(child), Some(root));
        assert_eq!(tree.children(child), &[grandchild]);
        assert_eq!(
            changes(&replica).last(),
            Some(&TaskChange::Parent(Some(child)))
        );
    }

    #[test]
    fn set_parent_to_none_moves_task_to_top_level() {
        let mut replica = replica();
        let [_, child, _] = family(&mut replica);

        replica.set_parent(child, None).unwrap();

        assert_eq!(replica.hierarchy().parent(child), None);
    }

    #[test]
    fn rejects_moving_task_under_itself_or_its_descendants() {
        let mut replica = replica();
        let [root, _, grandchild] = family(&mut replica);
        let pending_before = replica.pending_ops().len();

        assert_eq!(
            replica.set_parent(root, Some(grandchild)),
            Err(CommandError::Cycle {
                task: root,
                parent: grandchild
            })
        );
        assert_eq!(
            replica.set_parent(root, Some(root)),
            Err(CommandError::Cycle {
                task: root,
                parent: root
            })
        );
        assert_eq!(replica.pending_ops().len(), pending_before);
    }

    #[test]
    fn rejects_unknown_or_deleted_parent() {
        let mut replica = replica();
        let task = replica.create("task").unwrap();
        let gone = replica.create("gone").unwrap();
        replica.delete(gone).unwrap();

        assert_eq!(
            replica.set_parent(task, Some(TaskId(999))),
            Err(CommandError::NotFound(TaskId(999)))
        );
        assert_eq!(
            replica.set_parent(task, Some(gone)),
            Err(CommandError::Deleted(gone))
        );
    }

    #[test]
    fn tasks_under_a_deleted_task_cannot_be_edited() {
        let mut replica = replica();
        let [root, child, grandchild] = family(&mut replica);

        replica.delete(root).unwrap();

        assert_eq!(
            replica.rename(child, "a"),
            Err(CommandError::Deleted(child))
        );
        assert_eq!(
            replica.start(grandchild).map(drop),
            Err(CommandError::Deleted(grandchild))
        );
        assert!(replica.hierarchy().is_deleted(grandchild));
    }

    #[test]
    fn deleting_a_task_stops_timers_below_it() {
        let mut replica = replica();
        let [root, _, grandchild] = family(&mut replica);
        let timer = replica.start(grandchild).unwrap();

        replica.delete(root).unwrap();

        assert!(replica.pending_ops().iter().any(|op| matches!(
            op.target,
            OpTarget::TimeEntry {
                entry_id,
                change: TimeEntryChange::StoppedAt(Some(Timestamp(NOW))),
                ..
            } if entry_id == timer
        )));
        assert_eq!(replica.running_entry(), None);
    }

    #[test]
    fn remote_deletion_of_an_ancestor_stops_timer_when_it_happened() {
        let mut replica = replica();
        let [root, _, grandchild] = family(&mut replica);
        let timer = replica.start(grandchild).unwrap();

        let deletion = Op::task(
            OpId(900),
            root,
            Hlc::new(NOW + 50, 0, OTHER),
            TaskChange::Deleted(true),
        );
        replica.apply_remote(&deletion, Seq(1)).unwrap();

        assert_eq!(
            replica.time_entry(timer).unwrap().stopped_at(),
            Some(Timestamp(NOW + 50))
        );
    }

    #[test]
    fn time_under_a_deleted_task_is_not_tracked() {
        let mut replica = replica();
        let [root, child, _] = family(&mut replica);
        let timer = replica.start(child).unwrap();
        replica.stop(timer).unwrap();

        replica.delete(root).unwrap();

        assert!(replica.tracked_entries().is_empty());
        assert_eq!(
            replica.task_total(child, Timestamp(NOW)),
            std::time::Duration::ZERO
        );
    }
}
