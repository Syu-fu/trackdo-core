//! Commands on tasks.

use super::{CommandError, Replica};
use crate::env::{Clock, IdGen};
use crate::task::{TaskChange, TaskId, TaskStatus};
use crate::title::Title;

impl<C: Clock, G: IdGen> Replica<C, G> {
    pub fn create(&mut self, title: &str) -> Result<TaskId, CommandError> {
        let title = Title::new(title)?;
        let id = self.ids.task_id();
        self.emit_task(
            id,
            vec![
                TaskChange::Title(title),
                TaskChange::Status(TaskStatus::Todo),
            ],
        )?;
        Ok(id)
    }

    pub fn rename(&mut self, id: TaskId, title: &str) -> Result<(), CommandError> {
        self.ensure_editable(id)?;
        let title = Title::new(title)?;
        self.emit_task(id, vec![TaskChange::Title(title)])
    }

    pub fn complete(&mut self, id: TaskId) -> Result<(), CommandError> {
        self.ensure_editable(id)?;
        self.emit_task(id, vec![TaskChange::Status(TaskStatus::Done)])
    }

    pub fn reopen(&mut self, id: TaskId) -> Result<(), CommandError> {
        self.ensure_editable(id)?;
        self.emit_task(id, vec![TaskChange::Status(TaskStatus::Todo)])
    }

    pub fn delete(&mut self, id: TaskId) -> Result<(), CommandError> {
        self.ensure_editable(id)?;
        self.emit_task(id, vec![TaskChange::Deleted(true)])
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use crate::op::OpTarget;
    use crate::title::TitleError;

    #[test]
    fn create_adds_todo_task_with_title() {
        let mut replica = replica();

        let id = replica.create("  buy milk ").unwrap();

        let task = replica.task(id).unwrap();
        assert_eq!(task.title(), Some(&title("buy milk")));
        assert_eq!(task.status(), TaskStatus::Todo);
        assert!(!task.is_deleted());
    }

    #[test]
    fn create_emits_title_and_status_ops() {
        let mut replica = replica();

        let id = replica.create("buy milk").unwrap();

        assert_eq!(
            changes(&replica),
            vec![
                TaskChange::Title(title("buy milk")),
                TaskChange::Status(TaskStatus::Todo),
            ]
        );
        assert!(replica.pending_ops().iter().all(|op| matches!(
            op.target,
            OpTarget::Task { task_id, .. } if task_id == id
        )));
    }

    #[test]
    fn create_rejects_invalid_title_without_side_effects() {
        let mut replica = replica();

        let result = replica.create("   ");

        assert_eq!(result, Err(CommandError::Title(TitleError::Empty)));
        assert_eq!(replica.tasks().count(), 0);
        assert!(replica.pending_ops().is_empty());
    }

    #[test]
    fn emitted_ops_have_unique_ids_and_increasing_timestamps() {
        let mut replica = replica();

        let id = replica.create("a").unwrap();
        replica.rename(id, "b").unwrap();
        replica.complete(id).unwrap();

        let ops = replica.pending_ops();
        assert!(ops.windows(2).all(|w| w[0].hlc < w[1].hlc));
        assert!(ops.windows(2).all(|w| w[0].op_id != w[1].op_id));
        assert!(ops.iter().all(|op| op.hlc.device() == DEVICE));
        assert!(ops.iter().all(|op| op.hlc.wall_ms() == NOW));
    }

    #[test]
    fn rename_updates_title() {
        let mut replica = replica();
        let id = replica.create("old").unwrap();

        replica.rename(id, "new").unwrap();

        assert_eq!(replica.task(id).unwrap().title(), Some(&title("new")));
        assert_eq!(
            changes(&replica).last(),
            Some(&TaskChange::Title(title("new")))
        );
    }

    #[test]
    fn rename_rejects_invalid_title() {
        let mut replica = replica();
        let id = replica.create("old").unwrap();

        let result = replica.rename(id, "a\nb");

        assert_eq!(
            result,
            Err(CommandError::Title(TitleError::ContainsLineBreak))
        );
        assert_eq!(replica.task(id).unwrap().title(), Some(&title("old")));
    }

    #[test]
    fn complete_and_reopen_toggle_status() {
        let mut replica = replica();
        let id = replica.create("a").unwrap();

        replica.complete(id).unwrap();
        assert_eq!(replica.task(id).unwrap().status(), TaskStatus::Done);

        replica.reopen(id).unwrap();
        assert_eq!(replica.task(id).unwrap().status(), TaskStatus::Todo);
    }

    #[test]
    fn delete_marks_task_as_deleted() {
        let mut replica = replica();
        let id = replica.create("a").unwrap();

        replica.delete(id).unwrap();

        assert!(replica.task(id).unwrap().is_deleted());
        assert_eq!(changes(&replica).last(), Some(&TaskChange::Deleted(true)));
    }

    #[test]
    fn commands_on_unknown_task_fail() {
        let mut replica = replica();
        let unknown = TaskId(999);

        assert_eq!(
            replica.rename(unknown, "a"),
            Err(CommandError::NotFound(unknown))
        );
        assert_eq!(
            replica.complete(unknown),
            Err(CommandError::NotFound(unknown))
        );
        assert_eq!(
            replica.reopen(unknown),
            Err(CommandError::NotFound(unknown))
        );
        assert_eq!(
            replica.delete(unknown),
            Err(CommandError::NotFound(unknown))
        );
        assert!(replica.pending_ops().is_empty());
    }

    #[test]
    fn commands_on_deleted_task_fail() {
        let mut replica = replica();
        let id = replica.create("a").unwrap();
        replica.delete(id).unwrap();
        let pending_before = replica.pending_ops().len();

        assert_eq!(replica.rename(id, "b"), Err(CommandError::Deleted(id)));
        assert_eq!(replica.complete(id), Err(CommandError::Deleted(id)));
        assert_eq!(replica.reopen(id), Err(CommandError::Deleted(id)));
        assert_eq!(replica.delete(id), Err(CommandError::Deleted(id)));
        assert_eq!(replica.pending_ops().len(), pending_before);
    }
}
