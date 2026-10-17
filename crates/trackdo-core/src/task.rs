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
pub enum TaskChange {
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

    /// Rebuilds a task from previously exported registers.
    pub const fn from_registers(
        id: TaskId,
        title: Option<Lww<Title>>,
        status: Option<Lww<TaskStatus>>,
        deleted: Option<Lww<bool>>,
    ) -> Self {
        Self {
            id,
            title,
            status,
            deleted,
        }
    }

    pub const fn id(&self) -> TaskId {
        self.id
    }

    pub const fn title_register(&self) -> Option<&Lww<Title>> {
        self.title.as_ref()
    }

    pub const fn status_register(&self) -> Option<&Lww<TaskStatus>> {
        self.status.as_ref()
    }

    pub const fn deleted_register(&self) -> Option<&Lww<bool>> {
        self.deleted.as_ref()
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
    pub fn apply(&self, change: &TaskChange, hlc: Hlc) -> Self {
        match change {
            TaskChange::Title(title) => Self {
                title: Some(write(&self.title, title.clone(), hlc)),
                ..self.clone()
            },
            TaskChange::Status(status) => Self {
                status: Some(write(&self.status, *status, hlc)),
                ..self.clone()
            },
            TaskChange::Deleted(deleted) => Self {
                deleted: Some(write(&self.deleted, *deleted, hlc)),
                ..self.clone()
            },
        }
    }

    /// Timestamp of the most recent write to any field.
    pub(crate) fn newest_hlc(&self) -> Option<Hlc> {
        let title = self.title.as_ref().map(Lww::hlc);
        let status = self.status.as_ref().map(Lww::hlc);
        let deleted = self.deleted.as_ref().map(Lww::hlc);
        title.max(status).max(deleted)
    }

    /// Field-wise join with another copy of the same task, as received in a
    /// snapshot. Equivalent to applying every write either side has seen.
    pub(crate) fn merge(&self, other: &Self) -> Self {
        debug_assert_eq!(self.id, other.id, "merging different tasks");
        Self {
            id: self.id,
            title: join(&self.title, &other.title),
            status: join(&self.status, &other.status),
            deleted: join(&self.deleted, &other.deleted),
        }
    }
}

fn write<T: Clone + Ord>(field: &Option<Lww<T>>, value: T, hlc: Hlc) -> Lww<T> {
    match field {
        Some(current) => current.set(value, hlc),
        None => Lww::new(value, hlc),
    }
}

/// An unset field is the bottom element of the join.
fn join<T: Clone + Ord>(a: &Option<Lww<T>>, b: &Option<Lww<T>>) -> Option<Lww<T>> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.merge(b)),
        (Some(only), None) | (None, Some(only)) => Some(only.clone()),
        (None, None) => None,
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
            .apply(&TaskChange::Title(title("milk")), at(1, A))
            .apply(&TaskChange::Status(TaskStatus::Done), at(2, A))
            .apply(&TaskChange::Deleted(true), at(3, A));

        assert_eq!(task.title(), Some(&title("milk")));
        assert_eq!(task.status(), TaskStatus::Done);
        assert!(task.is_deleted());
    }

    #[test]
    fn apply_ignores_stale_write() {
        let task = Task::empty(ID)
            .apply(&TaskChange::Title(title("new")), at(200, A))
            .apply(&TaskChange::Title(title("old")), at(100, B));

        assert_eq!(task.title(), Some(&title("new")));
    }

    #[test]
    fn status_may_arrive_before_title() {
        let task = Task::empty(ID).apply(&TaskChange::Status(TaskStatus::Done), at(1, A));

        assert_eq!(task.title(), None);
        assert_eq!(task.status(), TaskStatus::Done);
    }

    #[test]
    fn concurrent_edits_to_different_fields_are_both_kept() {
        let base = Task::empty(ID).apply(&TaskChange::Title(title("milk")), at(1, A));

        let task = base
            .apply(&TaskChange::Title(title("2 milks")), at(10, A))
            .apply(&TaskChange::Status(TaskStatus::Done), at(5, B));

        assert_eq!(task.title(), Some(&title("2 milks")));
        assert_eq!(task.status(), TaskStatus::Done);
    }

    #[test]
    fn later_edit_does_not_resurrect_deleted_task() {
        let task = Task::empty(ID)
            .apply(&TaskChange::Title(title("milk")), at(1, A))
            .apply(&TaskChange::Deleted(true), at(2, A))
            .apply(&TaskChange::Title(title("renamed")), at(3, B));

        assert!(task.is_deleted());
        assert_eq!(task.title(), Some(&title("renamed")));
    }

    #[test]
    fn newer_undelete_restores_task() {
        let task = Task::empty(ID)
            .apply(&TaskChange::Deleted(true), at(2, A))
            .apply(&TaskChange::Deleted(false), at(3, A));

        assert!(!task.is_deleted());
    }

    fn change() -> impl Strategy<Value = TaskChange> {
        prop_oneof![
            prop::sample::select(vec!["a", "b", "c"]).prop_map(|s| TaskChange::Title(title(s))),
            prop::sample::select(vec![TaskStatus::Todo, TaskStatus::Done])
                .prop_map(TaskChange::Status),
            any::<bool>().prop_map(TaskChange::Deleted),
        ]
    }

    fn write() -> impl Strategy<Value = (TaskChange, Hlc)> {
        (change(), 0u64..4, 0u32..2, 0u128..3).prop_map(|(change, wall, counter, device)| {
            (change, Hlc::new(wall, counter, DeviceId(device)))
        })
    }

    fn apply_all(writes: &[(TaskChange, Hlc)]) -> Task {
        writes.iter().fold(Task::empty(ID), |task, (change, hlc)| {
            task.apply(change, *hlc)
        })
    }

    #[test]
    fn from_registers_exposes_registers() {
        let title_reg = Lww::new(title("milk"), at(1, A));
        let status_reg = Lww::new(TaskStatus::Done, at(2, A));
        let deleted_reg = Lww::new(true, at(3, B));

        let task = Task::from_registers(
            ID,
            Some(title_reg.clone()),
            Some(status_reg.clone()),
            Some(deleted_reg.clone()),
        );

        assert_eq!(task.title_register(), Some(&title_reg));
        assert_eq!(task.status_register(), Some(&status_reg));
        assert_eq!(task.deleted_register(), Some(&deleted_reg));
    }

    #[test]
    fn merge_keeps_newer_value_per_field() {
        let local = Task::empty(ID)
            .apply(&TaskChange::Title(title("local")), at(10, A))
            .apply(&TaskChange::Status(TaskStatus::Todo), at(1, A));
        let remote = Task::empty(ID)
            .apply(&TaskChange::Title(title("remote")), at(5, B))
            .apply(&TaskChange::Status(TaskStatus::Done), at(6, B));

        let merged = local.merge(&remote);

        assert_eq!(merged.title(), Some(&title("local")));
        assert_eq!(merged.status(), TaskStatus::Done);
        assert_eq!(merged.deleted_register(), None);
    }

    #[test]
    fn merge_with_empty_is_identity() {
        let task = Task::empty(ID).apply(&TaskChange::Title(title("a")), at(1, A));

        assert_eq!(task.merge(&Task::empty(ID)), task);
        assert_eq!(Task::empty(ID).merge(&task), task);
    }

    fn any_task() -> impl Strategy<Value = Task> {
        prop::collection::vec(write(), 0..6).prop_map(|writes| apply_all(&writes))
    }

    proptest! {
        #[test]
        fn merge_is_commutative(a in any_task(), b in any_task()) {
            prop_assert_eq!(a.merge(&b), b.merge(&a));
        }

        #[test]
        fn merge_is_associative(a in any_task(), b in any_task(), c in any_task()) {
            prop_assert_eq!(a.merge(&b).merge(&c), a.merge(&b.merge(&c)));
        }

        #[test]
        fn merge_is_idempotent(a in any_task()) {
            prop_assert_eq!(a.merge(&a), a);
        }

        #[test]
        fn merging_states_equals_applying_all_writes(
            left in prop::collection::vec(write(), 0..8),
            right in prop::collection::vec(write(), 0..8),
        ) {
            let both: Vec<_> = left.iter().chain(right.iter()).cloned().collect();

            prop_assert_eq!(apply_all(&left).merge(&apply_all(&right)), apply_all(&both));
        }

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
