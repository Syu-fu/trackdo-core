//! A replica: local task state plus the operations not yet sent.
//!
//! Commands validate user intent, emit operations and apply them locally at
//! once, so the UI never waits on the network.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::env::{Clock, IdGen};
use crate::hlc::{DeviceId, Hlc, HlcError};
use crate::op::Op;
use crate::task::{FieldChange, Task, TaskId, TaskStatus};
use crate::title::{Title, TitleError};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CommandError {
    #[error(transparent)]
    Title(#[from] TitleError),
    #[error("task {0:?} does not exist")]
    NotFound(TaskId),
    #[error("task {0:?} is deleted")]
    Deleted(TaskId),
    #[error(transparent)]
    Clock(#[from] HlcError),
}

pub struct Replica<C, G> {
    clock: C,
    ids: G,
    hlc: Hlc,
    tasks: BTreeMap<TaskId, Task>,
    pending: Vec<Op>,
}

impl<C: Clock, G: IdGen> Replica<C, G> {
    pub fn new(device: DeviceId, clock: C, ids: G) -> Self {
        Self {
            clock,
            ids,
            hlc: Hlc::zero(device),
            tasks: BTreeMap::new(),
            pending: Vec::new(),
        }
    }

    pub fn task(&self, id: TaskId) -> Option<&Task> {
        self.tasks.get(&id)
    }

    /// All known tasks, including deleted ones, ordered by id.
    pub fn tasks(&self) -> impl Iterator<Item = &Task> {
        self.tasks.values()
    }

    /// Operations applied locally but not yet acknowledged by the server.
    pub fn pending_ops(&self) -> &[Op] {
        &self.pending
    }

    pub fn create(&mut self, title: &str) -> Result<TaskId, CommandError> {
        let title = Title::new(title)?;
        let id = self.ids.task_id();
        self.emit(
            id,
            vec![
                FieldChange::Title(title),
                FieldChange::Status(TaskStatus::Todo),
            ],
        )?;
        Ok(id)
    }

    pub fn rename(&mut self, id: TaskId, title: &str) -> Result<(), CommandError> {
        self.ensure_editable(id)?;
        let title = Title::new(title)?;
        self.emit(id, vec![FieldChange::Title(title)])
    }

    pub fn complete(&mut self, id: TaskId) -> Result<(), CommandError> {
        self.ensure_editable(id)?;
        self.emit(id, vec![FieldChange::Status(TaskStatus::Done)])
    }

    pub fn reopen(&mut self, id: TaskId) -> Result<(), CommandError> {
        self.ensure_editable(id)?;
        self.emit(id, vec![FieldChange::Status(TaskStatus::Todo)])
    }

    pub fn delete(&mut self, id: TaskId) -> Result<(), CommandError> {
        self.ensure_editable(id)?;
        self.emit(id, vec![FieldChange::Deleted(true)])
    }

    fn ensure_editable(&self, id: TaskId) -> Result<(), CommandError> {
        match self.tasks.get(&id) {
            None => Err(CommandError::NotFound(id)),
            Some(task) if task.is_deleted() => Err(CommandError::Deleted(id)),
            Some(_) => Ok(()),
        }
    }

    /// Stamps `changes`, applies them locally and queues them for sending.
    /// All timestamps are computed first so a clock failure changes nothing.
    fn emit(&mut self, task_id: TaskId, changes: Vec<FieldChange>) -> Result<(), CommandError> {
        let now = self.clock.now_millis();
        let mut hlc = self.hlc;
        let mut stamped = Vec::with_capacity(changes.len());
        for change in changes {
            hlc = hlc.tick(now)?;
            stamped.push((change, hlc));
        }

        let current = self
            .tasks
            .get(&task_id)
            .cloned()
            .unwrap_or_else(|| Task::empty(task_id));
        let updated = stamped
            .iter()
            .fold(current, |task, (change, hlc)| task.apply(change, *hlc));

        self.hlc = hlc;
        self.tasks.insert(task_id, updated);
        for (change, hlc) in stamped {
            let op_id = self.ids.op_id();
            self.pending.push(Op {
                op_id,
                task_id,
                hlc,
                change,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op::OpId;

    const DEVICE: DeviceId = DeviceId(7);
    const NOW: u64 = 1_000;

    struct FixedClock(u64);

    impl Clock for FixedClock {
        fn now_millis(&self) -> u64 {
            self.0
        }
    }

    #[derive(Default)]
    struct SequentialIds(u128);

    impl IdGen for SequentialIds {
        fn task_id(&mut self) -> TaskId {
            self.0 += 1;
            TaskId(self.0)
        }

        fn op_id(&mut self) -> OpId {
            self.0 += 1;
            OpId(self.0)
        }
    }

    fn replica() -> Replica<FixedClock, SequentialIds> {
        Replica::new(DEVICE, FixedClock(NOW), SequentialIds::default())
    }

    fn title(s: &str) -> Title {
        Title::new(s).unwrap()
    }

    fn changes(replica: &Replica<FixedClock, SequentialIds>) -> Vec<FieldChange> {
        replica
            .pending_ops()
            .iter()
            .map(|op| op.change.clone())
            .collect()
    }

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
                FieldChange::Title(title("buy milk")),
                FieldChange::Status(TaskStatus::Todo),
            ]
        );
        assert!(replica.pending_ops().iter().all(|op| op.task_id == id));
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
            Some(&FieldChange::Title(title("new")))
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
        assert_eq!(changes(&replica).last(), Some(&FieldChange::Deleted(true)));
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
