//! Task aggregate built from independent last-writer-wins fields.

use crate::hlc::Hlc;
use crate::lww::Lww;
use crate::title::Title;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskId(pub u128);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TaskStatus {
    Todo,
    Done,
}

/// A write to a single task field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldChange {
    Title(Title),
    Status(TaskStatus),
    Deleted(bool),
}

/// Each field is `None` until its first write arrives, because there is no
/// dedicated create operation and writes may arrive in any order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    id: TaskId,
    title: Option<Lww<Title>>,
    status: Option<Lww<TaskStatus>>,
    deleted: Option<Lww<bool>>,
}

impl Task {
    /// A task that has not received any write yet.
    pub const fn empty(id: TaskId) -> Self {
        Self {
            id,
            title: None,
            status: None,
            deleted: None,
        }
    }

    pub const fn id(&self) -> TaskId {
        self.id
    }

    /// `None` until a title write has been received.
    pub fn title(&self) -> Option<&Title> {
        self.title.as_ref().map(Lww::value)
    }

    pub fn status(&self) -> TaskStatus {
        self.status
            .as_ref()
            .map_or(TaskStatus::Todo, |status| *status.value())
    }

    /// Deletion is a tombstone: edits to other fields never clear it.
    pub fn is_deleted(&self) -> bool {
        self.deleted
            .as_ref()
            .is_some_and(|deleted| *deleted.value())
    }

    /// Task after applying `change` written at `hlc`. Stale writes are ignored.
    pub fn apply(&self, change: &FieldChange, hlc: Hlc) -> Self {
        match change {
            FieldChange::Title(title) => Self {
                title: Some(write(&self.title, title.clone(), hlc)),
                ..self.clone()
            },
            FieldChange::Status(status) => Self {
                status: Some(write(&self.status, *status, hlc)),
                ..self.clone()
            },
            FieldChange::Deleted(deleted) => Self {
                deleted: Some(write(&self.deleted, *deleted, hlc)),
                ..self.clone()
            },
        }
    }
}

/// An unset field is the bottom element, so the first write always wins.
fn write<T: Clone + Ord>(field: &Option<Lww<T>>, value: T, hlc: Hlc) -> Lww<T> {
    match field {
        Some(current) => current.set(value, hlc),
        None => Lww::new(value, hlc),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hlc::DeviceId;
    use proptest::prelude::*;

    const ID: TaskId = TaskId(42);
    const A: DeviceId = DeviceId(1);
    const B: DeviceId = DeviceId(2);

    fn title(s: &str) -> Title {
        Title::new(s).unwrap()
    }

    fn at(wall: u64, device: DeviceId) -> Hlc {
        Hlc::new(wall, 0, device)
    }

    #[test]
    fn empty_task_has_defaults() {
        let task = Task::empty(ID);

        assert_eq!(task.id(), ID);
        assert_eq!(task.title(), None);
        assert_eq!(task.status(), TaskStatus::Todo);
        assert!(!task.is_deleted());
    }

    #[test]
    fn apply_sets_each_field() {
        let task = Task::empty(ID)
            .apply(&FieldChange::Title(title("milk")), at(1, A))
            .apply(&FieldChange::Status(TaskStatus::Done), at(2, A))
            .apply(&FieldChange::Deleted(true), at(3, A));

        assert_eq!(task.title(), Some(&title("milk")));
        assert_eq!(task.status(), TaskStatus::Done);
        assert!(task.is_deleted());
    }

    #[test]
    fn apply_ignores_stale_write() {
        let task = Task::empty(ID)
            .apply(&FieldChange::Title(title("new")), at(200, A))
            .apply(&FieldChange::Title(title("old")), at(100, B));

        assert_eq!(task.title(), Some(&title("new")));
    }

    #[test]
    fn status_may_arrive_before_title() {
        let task = Task::empty(ID).apply(&FieldChange::Status(TaskStatus::Done), at(1, A));

        assert_eq!(task.title(), None);
        assert_eq!(task.status(), TaskStatus::Done);
    }

    #[test]
    fn concurrent_edits_to_different_fields_are_both_kept() {
        let base = Task::empty(ID).apply(&FieldChange::Title(title("milk")), at(1, A));

        let task = base
            .apply(&FieldChange::Title(title("2 milks")), at(10, A))
            .apply(&FieldChange::Status(TaskStatus::Done), at(5, B));

        assert_eq!(task.title(), Some(&title("2 milks")));
        assert_eq!(task.status(), TaskStatus::Done);
    }

    #[test]
    fn later_edit_does_not_resurrect_deleted_task() {
        let task = Task::empty(ID)
            .apply(&FieldChange::Title(title("milk")), at(1, A))
            .apply(&FieldChange::Deleted(true), at(2, A))
            .apply(&FieldChange::Title(title("renamed")), at(3, B));

        assert!(task.is_deleted());
        assert_eq!(task.title(), Some(&title("renamed")));
    }

    #[test]
    fn newer_undelete_restores_task() {
        let task = Task::empty(ID)
            .apply(&FieldChange::Deleted(true), at(2, A))
            .apply(&FieldChange::Deleted(false), at(3, A));

        assert!(!task.is_deleted());
    }

    fn change() -> impl Strategy<Value = FieldChange> {
        prop_oneof![
            prop::sample::select(vec!["a", "b", "c"]).prop_map(|s| FieldChange::Title(title(s))),
            prop::sample::select(vec![TaskStatus::Todo, TaskStatus::Done])
                .prop_map(FieldChange::Status),
            any::<bool>().prop_map(FieldChange::Deleted),
        ]
    }

    fn write() -> impl Strategy<Value = (FieldChange, Hlc)> {
        (change(), 0u64..4, 0u32..2, 0u128..3).prop_map(|(change, wall, counter, device)| {
            (change, Hlc::new(wall, counter, DeviceId(device)))
        })
    }

    fn apply_all(writes: &[(FieldChange, Hlc)]) -> Task {
        writes.iter().fold(Task::empty(ID), |task, (change, hlc)| {
            task.apply(change, *hlc)
        })
    }

    proptest! {
        #[test]
        fn apply_order_does_not_matter(
            (writes, shuffled) in prop::collection::vec(write(), 0..12)
                .prop_flat_map(|w| (Just(w.clone()), Just(w).prop_shuffle()))
        ) {
            prop_assert_eq!(apply_all(&writes), apply_all(&shuffled));
        }

        #[test]
        fn reapplying_writes_is_idempotent(writes in prop::collection::vec(write(), 0..12)) {
            let doubled: Vec<_> = writes.iter().chain(writes.iter()).cloned().collect();

            prop_assert_eq!(apply_all(&writes), apply_all(&doubled));
        }
    }
}
