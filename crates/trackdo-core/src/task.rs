//! Task aggregate built from independent last-writer-wins fields.

use std::collections::BTreeMap;

use crate::date::Date;
use crate::hlc::Hlc;
use crate::lww::{Lww, join, write};
use crate::note::Note;
use crate::tag::TagId;
use crate::time_entry::Timestamp;
use crate::title::Title;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskId(pub u128);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TaskStatus {
    Todo,
    Done,
}

/// Ordered from least to most urgent, so tasks can be sorted by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TaskPriority {
    None,
    Low,
    Medium,
    High,
}

/// A write to a single task field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskChange {
    Title(Title),
    Status(TaskStatus),
    Deleted(bool),
    Priority(TaskPriority),
    Note(Note),
    /// `None` clears the due date.
    Due(Option<Date>),
    /// When the task is planned to be worked on; `None` unschedules it.
    ScheduledAt(Option<Timestamp>),
    /// `None` moves the task to the top level.
    Parent(Option<TaskId>),
    /// Adds or removes one tag, leaving the others as they are.
    Tag {
        tag: TagId,
        tagged: bool,
    },
}

/// The register of every task field. Each is `None` until its first write
/// arrives, because there is no dedicated create operation and writes may
/// arrive in any order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskRegisters {
    pub title: Option<Lww<Title>>,
    pub status: Option<Lww<TaskStatus>>,
    pub deleted: Option<Lww<bool>>,
    pub priority: Option<Lww<TaskPriority>>,
    pub note: Option<Lww<Note>>,
    pub due: Option<Lww<Option<Date>>>,
    pub scheduled_at: Option<Lww<Option<Timestamp>>>,
    pub parent: Option<Lww<Option<TaskId>>>,
    /// Whether each tag ever written is on the task.
    pub tags: BTreeMap<TagId, Lww<bool>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    id: TaskId,
    registers: TaskRegisters,
}

impl Task {
    /// A task that has not received any write yet.
    pub const fn empty(id: TaskId) -> Self {
        Self {
            id,
            registers: TaskRegisters {
                title: None,
                status: None,
                deleted: None,
                priority: None,
                note: None,
                due: None,
                scheduled_at: None,
                parent: None,
                tags: BTreeMap::new(),
            },
        }
    }

    /// Rebuilds a task from previously exported registers.
    pub const fn from_registers(id: TaskId, registers: TaskRegisters) -> Self {
        Self { id, registers }
    }

    pub const fn id(&self) -> TaskId {
        self.id
    }

    pub const fn registers(&self) -> &TaskRegisters {
        &self.registers
    }

    /// `None` until a title write has been received.
    pub fn title(&self) -> Option<&Title> {
        self.registers.title.as_ref().map(Lww::value)
    }

    pub fn status(&self) -> TaskStatus {
        self.registers
            .status
            .as_ref()
            .map_or(TaskStatus::Todo, |status| *status.value())
    }

    pub fn priority(&self) -> TaskPriority {
        self.registers
            .priority
            .as_ref()
            .map_or(TaskPriority::None, |priority| *priority.value())
    }

    /// `None` while no note or only an empty one has been written.
    pub fn note(&self) -> Option<&Note> {
        self.registers
            .note
            .as_ref()
            .map(Lww::value)
            .filter(|note| !note.is_empty())
    }

    pub fn due(&self) -> Option<Date> {
        self.registers.due.as_ref().and_then(|due| *due.value())
    }

    pub fn scheduled_at(&self) -> Option<Timestamp> {
        self.registers
            .scheduled_at
            .as_ref()
            .and_then(|scheduled| *scheduled.value())
    }

    /// The parent as written. The parent that takes effect may differ; see
    /// [`crate::hierarchy::Hierarchy`].
    pub fn parent(&self) -> Option<TaskId> {
        self.registers
            .parent
            .as_ref()
            .and_then(|parent| *parent.value())
    }

    /// Ids of the tags on the task, including tags that are deleted or not
    /// received yet.
    pub fn tag_ids(&self) -> impl Iterator<Item = TagId> + '_ {
        self.registers
            .tags
            .iter()
            .filter(|(_, tagged)| *tagged.value())
            .map(|(&tag, _)| tag)
    }

    /// Deletion is a tombstone: edits to other fields never clear it.
    pub fn is_deleted(&self) -> bool {
        self.registers
            .deleted
            .as_ref()
            .is_some_and(|deleted| *deleted.value())
    }

    /// Task after applying `change` written at `hlc`. Stale writes are ignored.
    pub fn apply(&self, change: &TaskChange, hlc: Hlc) -> Self {
        let current = &self.registers;
        let registers = match change {
            TaskChange::Title(title) => TaskRegisters {
                title: Some(write(&current.title, title.clone(), hlc)),
                ..current.clone()
            },
            TaskChange::Status(status) => TaskRegisters {
                status: Some(write(&current.status, *status, hlc)),
                ..current.clone()
            },
            TaskChange::Deleted(deleted) => TaskRegisters {
                deleted: Some(write(&current.deleted, *deleted, hlc)),
                ..current.clone()
            },
            TaskChange::Priority(priority) => TaskRegisters {
                priority: Some(write(&current.priority, *priority, hlc)),
                ..current.clone()
            },
            TaskChange::Note(note) => TaskRegisters {
                note: Some(write(&current.note, note.clone(), hlc)),
                ..current.clone()
            },
            TaskChange::Due(due) => TaskRegisters {
                due: Some(write(&current.due, *due, hlc)),
                ..current.clone()
            },
            TaskChange::ScheduledAt(scheduled) => TaskRegisters {
                scheduled_at: Some(write(&current.scheduled_at, *scheduled, hlc)),
                ..current.clone()
            },
            TaskChange::Parent(parent) => TaskRegisters {
                parent: Some(write(&current.parent, *parent, hlc)),
                ..current.clone()
            },
            TaskChange::Tag { tag, tagged } => {
                let mut tags = current.tags.clone();
                tags.insert(*tag, write(&current.tags.get(tag).cloned(), *tagged, hlc));
                TaskRegisters {
                    tags,
                    ..current.clone()
                }
            }
        };
        Self {
            id: self.id,
            registers,
        }
    }

    /// The write each field holds, as it would be sent again.
    pub(crate) fn writes(&self) -> impl Iterator<Item = (TaskChange, Hlc)> {
        let TaskRegisters {
            title,
            status,
            deleted,
            priority,
            note,
            due,
            scheduled_at,
            parent,
            tags,
        } = &self.registers;
        let title = title
            .as_ref()
            .map(|title| (TaskChange::Title(title.value().clone()), title.hlc()));
        let status = status
            .as_ref()
            .map(|status| (TaskChange::Status(*status.value()), status.hlc()));
        let deleted = deleted
            .as_ref()
            .map(|deleted| (TaskChange::Deleted(*deleted.value()), deleted.hlc()));
        let priority = priority
            .as_ref()
            .map(|priority| (TaskChange::Priority(*priority.value()), priority.hlc()));
        let note = note
            .as_ref()
            .map(|note| (TaskChange::Note(note.value().clone()), note.hlc()));
        let due = due
            .as_ref()
            .map(|due| (TaskChange::Due(*due.value()), due.hlc()));
        let scheduled_at = scheduled_at
            .as_ref()
            .map(|scheduled| (TaskChange::ScheduledAt(*scheduled.value()), scheduled.hlc()));
        let parent = parent
            .as_ref()
            .map(|parent| (TaskChange::Parent(*parent.value()), parent.hlc()));
        [
            title,
            status,
            deleted,
            priority,
            note,
            due,
            scheduled_at,
            parent,
        ]
        .into_iter()
        .flatten()
        .chain(tags.iter().map(|(&tag, tagged)| {
            let change = TaskChange::Tag {
                tag,
                tagged: *tagged.value(),
            };
            (change, tagged.hlc())
        }))
    }

    /// Timestamp of the most recent write to any field.
    pub(crate) fn newest_hlc(&self) -> Option<Hlc> {
        self.writes().map(|(_, hlc)| hlc).max()
    }

    /// Field-wise join with another copy of the same task, as received in a
    /// snapshot. Equivalent to applying every write either side has seen.
    pub(crate) fn merge(&self, other: &Self) -> Self {
        debug_assert_eq!(self.id, other.id, "merging different tasks");
        let (a, b) = (&self.registers, &other.registers);
        Self {
            id: self.id,
            registers: TaskRegisters {
                title: join(&a.title, &b.title),
                status: join(&a.status, &b.status),
                deleted: join(&a.deleted, &b.deleted),
                priority: join(&a.priority, &b.priority),
                note: join(&a.note, &b.note),
                due: join(&a.due, &b.due),
                scheduled_at: join(&a.scheduled_at, &b.scheduled_at),
                parent: join(&a.parent, &b.parent),
                tags: join_tags(&a.tags, &b.tags),
            },
        }
    }
}

/// Key-wise join; a tag only one side has seen is taken as is.
fn join_tags(
    a: &BTreeMap<TagId, Lww<bool>>,
    b: &BTreeMap<TagId, Lww<bool>>,
) -> BTreeMap<TagId, Lww<bool>> {
    let mut joined = a.clone();
    for (tag, register) in b {
        let merged = join(&joined.get(tag).cloned(), &Some(register.clone()));
        joined.extend(merged.map(|merged| (*tag, merged)));
    }
    joined
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

    fn date(day: u8) -> Date {
        Date::new(2026, 10, day).unwrap()
    }

    fn tagged(tag: TagId, tagged: bool) -> TaskChange {
        TaskChange::Tag { tag, tagged }
    }

    fn note(s: &str) -> Note {
        Note::new(s).unwrap()
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
    fn priority_defaults_to_none_and_follows_newest_write() {
        let task = Task::empty(ID);
        assert_eq!(task.priority(), TaskPriority::None);

        let task = task
            .apply(&TaskChange::Priority(TaskPriority::High), at(2, A))
            .apply(&TaskChange::Priority(TaskPriority::Low), at(1, B));

        assert_eq!(task.priority(), TaskPriority::High);
    }

    #[test]
    fn note_follows_newest_write_and_empty_means_none() {
        let task = Task::empty(ID).apply(&TaskChange::Note(note("call first")), at(1, A));
        assert_eq!(task.note(), Some(&note("call first")));

        let task = task.apply(&TaskChange::Note(Note::default()), at(2, A));

        assert_eq!(task.note(), None);
        assert!(task.registers().note.is_some());
    }

    #[test]
    fn due_date_follows_newest_write_and_can_be_cleared() {
        let task = Task::empty(ID);
        assert_eq!(task.due(), None);

        let task = task.apply(&TaskChange::Due(Some(date(25))), at(1, A));
        assert_eq!(task.due(), Some(date(25)));

        let task = task.apply(&TaskChange::Due(None), at(2, B));
        assert_eq!(task.due(), None);
    }

    #[test]
    fn scheduled_time_follows_newest_write_and_can_be_cleared() {
        let task = Task::empty(ID);
        assert_eq!(task.scheduled_at(), None);

        let task = task.apply(&TaskChange::ScheduledAt(Some(Timestamp(900))), at(1, A));
        assert_eq!(task.scheduled_at(), Some(Timestamp(900)));

        let task = task.apply(&TaskChange::ScheduledAt(None), at(2, B));
        assert_eq!(task.scheduled_at(), None);
    }

    #[test]
    fn each_tag_is_tagged_by_its_newest_write() {
        let (work, home) = (TagId(1), TagId(2));
        let task = Task::empty(ID)
            .apply(&tagged(work, true), at(1, A))
            .apply(&tagged(home, true), at(1, B))
            .apply(&tagged(work, false), at(3, B))
            .apply(&tagged(work, true), at(2, A));

        assert_eq!(task.tag_ids().collect::<Vec<_>>(), vec![home]);
        assert_eq!(task.registers().tags.len(), 2);
    }

    #[test]
    fn parent_follows_newest_write_and_can_be_cleared() {
        let task = Task::empty(ID);
        assert_eq!(task.parent(), None);

        let task = task.apply(&TaskChange::Parent(Some(TaskId(7))), at(1, A));
        assert_eq!(task.parent(), Some(TaskId(7)));

        let task = task.apply(&TaskChange::Parent(None), at(2, B));
        assert_eq!(task.parent(), None);
    }

    #[test]
    fn priorities_order_from_none_to_high() {
        assert!(TaskPriority::None < TaskPriority::Low);
        assert!(TaskPriority::Low < TaskPriority::Medium);
        assert!(TaskPriority::Medium < TaskPriority::High);
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
            prop::sample::select(vec![
                TaskPriority::None,
                TaskPriority::Low,
                TaskPriority::Medium,
                TaskPriority::High,
            ])
            .prop_map(TaskChange::Priority),
            prop::sample::select(vec!["", "x", "y\nz"]).prop_map(|s| TaskChange::Note(note(s))),
            prop::option::of(1u8..4).prop_map(|day| TaskChange::Due(day.map(date))),
            prop::option::of(0u64..3).prop_map(|time| TaskChange::ScheduledAt(time.map(Timestamp))),
            prop::option::of(0u128..3).prop_map(|id| TaskChange::Parent(id.map(TaskId))),
            (0u128..3, any::<bool>()).prop_map(|(tag, on)| tagged(TagId(tag), on)),
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

        let registers = TaskRegisters {
            title: Some(title_reg),
            status: Some(status_reg),
            deleted: Some(deleted_reg),
            priority: Some(Lww::new(TaskPriority::High, at(4, A))),
            note: Some(Lww::new(note("n"), at(5, B))),
            due: Some(Lww::new(Some(date(1)), at(6, A))),
            scheduled_at: Some(Lww::new(Some(Timestamp(9)), at(7, B))),
            parent: Some(Lww::new(Some(TaskId(3)), at(8, A))),
            tags: BTreeMap::from([(TagId(1), Lww::new(true, at(9, B)))]),
        };

        let task = Task::from_registers(ID, registers.clone());

        assert_eq!(task.registers(), &registers);
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
        assert_eq!(merged.registers().deleted, None);
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
