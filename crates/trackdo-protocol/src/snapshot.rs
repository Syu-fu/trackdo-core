//! Wire representation of a full state snapshot (`GET /snapshot`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use trackdo_core::lww::Lww;
use trackdo_core::op::{LogId, Seq};
use trackdo_core::recurrence::Recurrence;
use trackdo_core::replica::Snapshot;
use trackdo_core::tag::{Tag, TagId};
use trackdo_core::task::{Task, TaskId, TaskPriority, TaskRegisters, TaskStatus};
use trackdo_core::time_entry::{TimeEntry, TimeEntryId, Timestamp};

use crate::date::{decode_date, encode_date};
use crate::error::{ProtocolError, decode_note, decode_title};
use crate::op::{HlcDto, TaskPriorityDto, TaskStatusDto};
use crate::recurrence::RecurrenceDto;
use crate::tag::TagDto;

/// All tasks, time entries and tags of the log `log_id` as of `seq`. `seq`
/// is `null` when the log is still empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotDto {
    pub log_id: Uuid,
    pub seq: Option<u64>,
    pub tasks: Vec<TaskDto>,
    pub time_entries: Vec<TimeEntryDto>,
    pub tags: Vec<TagDto>,
}

/// A task with every field's register, so receivers can merge it exactly.
///
/// Fields added after v0.3 may be missing, so states stored by older
/// versions still load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDto {
    pub id: Uuid,
    pub title: Option<RegisterDto<String>>,
    pub status: Option<RegisterDto<TaskStatusDto>>,
    pub deleted: Option<RegisterDto<bool>>,
    #[serde(default)]
    pub priority: Option<RegisterDto<TaskPriorityDto>>,
    #[serde(default)]
    pub note: Option<RegisterDto<String>>,
    /// A `null` value means the due date was cleared.
    #[serde(default)]
    pub due: Option<RegisterDto<Option<String>>>,
    /// Unix milliseconds; a `null` value means the task was unscheduled.
    #[serde(default)]
    pub scheduled_at: Option<RegisterDto<Option<u64>>>,
    /// A `null` value means the task was moved to the top level.
    #[serde(default)]
    pub parent: Option<RegisterDto<Option<Uuid>>>,
    /// A `null` value means the task stopped repeating.
    #[serde(default)]
    pub recurrence: Option<RegisterDto<Option<RecurrenceDto>>>,
    /// One register per tag ever written to the task, ordered by tag id.
    #[serde(default)]
    pub tags: Vec<TaskTagDto>,
}

/// Whether one tag is on a task, as of `hlc`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskTagDto {
    pub tag_id: Uuid,
    pub tagged: bool,
    pub hlc: HlcDto,
}

/// A time entry with every field's register. Times are Unix milliseconds.
///
/// A `stopped_at` register whose value is `null` marks a running timer, while
/// a `null` register means no stop or start write has been seen yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeEntryDto {
    pub id: Uuid,
    pub task_id: Uuid,
    pub started_at: Option<RegisterDto<u64>>,
    pub stopped_at: Option<RegisterDto<Option<u64>>>,
    pub deleted: Option<RegisterDto<bool>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterDto<T> {
    pub value: T,
    pub hlc: HlcDto,
}

impl From<&Snapshot> for SnapshotDto {
    fn from(snapshot: &Snapshot) -> Self {
        Self {
            log_id: Uuid::from_u128(snapshot.log_id.0),
            seq: snapshot.seq.map(|Seq(seq)| seq),
            tasks: snapshot.tasks.iter().map(TaskDto::from).collect(),
            time_entries: snapshot
                .time_entries
                .iter()
                .map(TimeEntryDto::from)
                .collect(),
            tags: snapshot.tags.iter().map(TagDto::from).collect(),
        }
    }
}

/// Validates the snapshot for `Replica::apply_snapshot`.
impl TryFrom<SnapshotDto> for Snapshot {
    type Error = ProtocolError;

    fn try_from(dto: SnapshotDto) -> Result<Self, Self::Error> {
        let tasks = dto
            .tasks
            .into_iter()
            .map(Task::try_from)
            .collect::<Result<_, _>>()?;
        Ok(Self {
            log_id: LogId(dto.log_id.as_u128()),
            tasks,
            time_entries: dto.time_entries.into_iter().map(TimeEntry::from).collect(),
            tags: dto
                .tags
                .into_iter()
                .map(Tag::try_from)
                .collect::<Result<_, _>>()?,
            seq: dto.seq.map(Seq),
        })
    }
}

impl From<&Task> for TaskDto {
    fn from(task: &Task) -> Self {
        let registers = task.registers();
        Self {
            id: Uuid::from_u128(task.id().0),
            title: registers
                .title
                .as_ref()
                .map(|reg| encode(reg, |title| title.as_str().to_owned())),
            status: registers
                .status
                .as_ref()
                .map(|reg| encode(reg, |status| TaskStatusDto::from(*status))),
            deleted: registers
                .deleted
                .as_ref()
                .map(|reg| encode(reg, |deleted| *deleted)),
            priority: registers
                .priority
                .as_ref()
                .map(|reg| encode(reg, |priority| TaskPriorityDto::from(*priority))),
            note: registers
                .note
                .as_ref()
                .map(|reg| encode(reg, |note| note.as_str().to_owned())),
            due: registers
                .due
                .as_ref()
                .map(|reg| encode(reg, |due| due.map(encode_date))),
            scheduled_at: registers.scheduled_at.as_ref().map(|reg| {
                encode(reg, |scheduled| {
                    scheduled.map(|Timestamp(scheduled)| scheduled)
                })
            }),
            parent: registers.parent.as_ref().map(|reg| {
                encode(reg, |parent| {
                    parent.map(|TaskId(parent)| Uuid::from_u128(parent))
                })
            }),
            recurrence: registers.recurrence.as_ref().map(|reg| {
                encode(reg, |recurrence| {
                    recurrence.as_ref().map(RecurrenceDto::from)
                })
            }),
            tags: registers
                .tags
                .iter()
                .map(|(TagId(tag), tagged)| TaskTagDto {
                    tag_id: Uuid::from_u128(*tag),
                    tagged: *tagged.value(),
                    hlc: HlcDto::from(tagged.hlc()),
                })
                .collect(),
        }
    }
}

impl TryFrom<TaskDto> for Task {
    type Error = ProtocolError;

    fn try_from(dto: TaskDto) -> Result<Self, Self::Error> {
        let title = dto
            .title
            .map(|reg| Ok::<_, ProtocolError>(Lww::new(decode_title(&reg.value)?, reg.hlc.into())))
            .transpose()?;
        let note = dto
            .note
            .map(|reg| Ok::<_, ProtocolError>(Lww::new(decode_note(&reg.value)?, reg.hlc.into())))
            .transpose()?;
        let due = dto
            .due
            .map(|reg| {
                let due = reg.value.as_deref().map(decode_date).transpose()?;
                Ok::<_, ProtocolError>(Lww::new(due, reg.hlc.into()))
            })
            .transpose()?;
        let recurrence = dto
            .recurrence
            .map(|reg| {
                let recurrence = reg.value.map(Recurrence::try_from).transpose()?;
                Ok::<_, ProtocolError>(Lww::new(recurrence, reg.hlc.into()))
            })
            .transpose()?;
        Ok(Self::from_registers(
            TaskId(dto.id.as_u128()),
            TaskRegisters {
                title,
                status: dto.status.map(|reg| decode(reg, TaskStatus::from)),
                deleted: dto.deleted.map(|reg| decode(reg, |deleted| deleted)),
                priority: dto.priority.map(|reg| decode(reg, TaskPriority::from)),
                note,
                due,
                scheduled_at: dto
                    .scheduled_at
                    .map(|reg| decode(reg, |scheduled| scheduled.map(Timestamp))),
                parent: dto
                    .parent
                    .map(|reg| decode(reg, |parent| parent.map(|parent| TaskId(parent.as_u128())))),
                recurrence,
                tags: decode_task_tags(dto.tags),
            },
        ))
    }
}

impl From<&TimeEntry> for TimeEntryDto {
    fn from(entry: &TimeEntry) -> Self {
        Self {
            id: Uuid::from_u128(entry.id().0),
            task_id: Uuid::from_u128(entry.task_id().0),
            started_at: entry
                .started_at_register()
                .map(|reg| encode(reg, |Timestamp(started)| *started)),
            stopped_at: entry
                .stopped_at_register()
                .map(|reg| encode(reg, |stopped| stopped.map(|Timestamp(stopped)| stopped))),
            deleted: entry
                .deleted_register()
                .map(|reg| encode(reg, |deleted| *deleted)),
        }
    }
}

impl From<TimeEntryDto> for TimeEntry {
    fn from(dto: TimeEntryDto) -> Self {
        Self::from_registers(
            TimeEntryId(dto.id.as_u128()),
            TaskId(dto.task_id.as_u128()),
            dto.started_at.map(|reg| decode(reg, Timestamp)),
            dto.stopped_at
                .map(|reg| decode(reg, |stopped| stopped.map(Timestamp))),
            dto.deleted.map(|reg| decode(reg, |deleted| deleted)),
        )
    }
}

/// A tag listed twice keeps its newer register, as merging would.
fn decode_task_tags(tags: Vec<TaskTagDto>) -> BTreeMap<TagId, Lww<bool>> {
    tags.into_iter().fold(BTreeMap::new(), |mut decoded, tag| {
        let id = TagId(tag.tag_id.as_u128());
        let register = Lww::new(tag.tagged, tag.hlc.into());
        let merged = match decoded.remove(&id) {
            Some(existing) => existing.merge(&register),
            None => register,
        };
        decoded.insert(id, merged);
        decoded
    })
}

fn encode<T, U>(register: &Lww<T>, value: impl FnOnce(&T) -> U) -> RegisterDto<U>
where
    T: Clone + Ord,
{
    RegisterDto {
        value: value(register.value()),
        hlc: HlcDto::from(register.hlc()),
    }
}

fn decode<T, U>(register: RegisterDto<T>, value: impl FnOnce(T) -> U) -> Lww<U>
where
    U: Clone + Ord,
{
    Lww::new(value(register.value), register.hlc.into())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use proptest::prelude::*;
    use trackdo_core::date::Date;
    use trackdo_core::hlc::{DeviceId, Hlc};
    use trackdo_core::note::Note;
    use trackdo_core::task::TaskChange;
    use trackdo_core::time_entry::TimeEntryChange;
    use trackdo_core::title::Title;

    const DEVICE: DeviceId = DeviceId(0x0192_8f39_ffff_7abc_9def_0123_4567_89ab);
    const TASK: TaskId = TaskId(0x0192_8f3a_0000_7000_8000_0000_0000_0001);
    const LOG: LogId = LogId(0x0192_8f30_0000_7000_8000_0000_0000_0001);
    const LOG_UUID: &str = "01928f30-0000-7000-8000-000000000001";

    fn at(wall: u64) -> Hlc {
        Hlc::new(wall, 0, DEVICE)
    }

    fn title(s: &str) -> Title {
        Title::new(s).unwrap()
    }

    pub(crate) fn full_task() -> Task {
        Task::empty(TASK)
            .apply(&TaskChange::Title(title("buy milk")), at(1_730_000_000_000))
            .apply(&TaskChange::Status(TaskStatus::Done), at(1_730_000_000_500))
            .apply(&TaskChange::Deleted(false), at(1_730_000_000_900))
            .apply(
                &TaskChange::Priority(TaskPriority::High),
                at(1_730_000_000_950),
            )
            .apply(
                &TaskChange::Note(Note::new("oat milk if out").unwrap()),
                at(1_730_000_000_990),
            )
            .apply(
                &TaskChange::Due(Some(Date::new(2024, 10, 31).unwrap())),
                at(1_730_000_000_995),
            )
            .apply(
                &TaskChange::ScheduledAt(Some(Timestamp(1_730_100_000_000))),
                at(1_730_000_000_999),
            )
            .apply(
                &TaskChange::Recurrence(Some(crate::recurrence::tests::weekly())),
                at(1_730_000_001_000),
            )
    }

    fn partial_task() -> Task {
        Task::empty(TaskId(0x0192_8f3a_0000_7000_8000_0000_0000_0002))
            .apply(&TaskChange::Status(TaskStatus::Todo), at(1_730_000_001_000))
            .apply(&TaskChange::Parent(Some(TASK)), at(1_730_000_001_000))
            .apply(
                &TaskChange::Tag {
                    tag: crate::tag::tests::work_tag().id(),
                    tagged: true,
                },
                at(1_730_000_001_000),
            )
    }

    fn stopped_entry() -> TimeEntry {
        TimeEntry::empty(TimeEntryId(0x0192_8f3a_0000_7000_8000_0000_0000_0e01), TASK)
            .apply(
                &TimeEntryChange::StartedAt(Timestamp(1_730_000_000_000)),
                at(1_730_000_000_000),
            )
            .apply(
                &TimeEntryChange::StoppedAt(Some(Timestamp(1_730_000_360_000))),
                at(1_730_000_360_000),
            )
            .apply(&TimeEntryChange::Deleted(false), at(1_730_000_360_000))
    }

    pub(crate) fn running_entry() -> TimeEntry {
        TimeEntry::empty(TimeEntryId(0x0192_8f3a_0000_7000_8000_0000_0000_0e02), TASK)
            .apply(
                &TimeEntryChange::StartedAt(Timestamp(1_730_000_400_000)),
                at(1_730_000_400_000),
            )
            .apply(&TimeEntryChange::StoppedAt(None), at(1_730_000_400_000))
    }

    #[test]
    fn snapshot_wire_format() {
        insta::assert_json_snapshot!(SnapshotDto::from(&Snapshot {
            log_id: LOG,
            tasks: vec![full_task(), partial_task()],
            time_entries: vec![stopped_entry(), running_entry()],
            tags: vec![crate::tag::tests::work_tag()],
            seq: Some(Seq(42)),
        }));
    }

    #[test]
    fn empty_snapshot_wire_format() {
        insta::assert_json_snapshot!(SnapshotDto::from(&Snapshot {
            log_id: LOG,
            tasks: Vec::new(),
            time_entries: Vec::new(),
            tags: Vec::new(),
            seq: None,
        }));
    }

    #[test]
    fn distinguishes_running_entry_from_unknown_stop() {
        let unknown_stop = TimeEntry::empty(TimeEntryId(3), TASK)
            .apply(&TimeEntryChange::StartedAt(Timestamp(10)), at(10));
        let snapshot = Snapshot {
            log_id: LOG,
            tasks: Vec::new(),
            time_entries: vec![running_entry(), unknown_stop],
            tags: Vec::new(),
            seq: None,
        };
        let json = serde_json::to_value(SnapshotDto::from(&snapshot)).unwrap();

        assert_eq!(
            json["time_entries"][0]["stopped_at"]["value"],
            serde_json::Value::Null
        );
        assert_eq!(
            json["time_entries"][1]["stopped_at"],
            serde_json::Value::Null
        );
        let dto: SnapshotDto = serde_json::from_value(json).unwrap();
        assert_eq!(Snapshot::try_from(dto), Ok(snapshot));
    }

    #[test]
    fn rejects_snapshot_without_log_id() {
        let json = serde_json::json!({ "seq": null, "tasks": [], "time_entries": [] });

        assert!(serde_json::from_value::<SnapshotDto>(json).is_err());
    }

    #[test]
    fn rejects_snapshot_without_tags() {
        let json = serde_json::json!({
            "log_id": LOG_UUID, "seq": null, "tasks": [], "time_entries": [],
        });

        assert!(serde_json::from_value::<SnapshotDto>(json).is_err());
    }

    #[test]
    fn rejects_snapshot_without_time_entries() {
        let json = serde_json::json!({ "log_id": LOG_UUID, "seq": null, "tasks": [] });

        assert!(serde_json::from_value::<SnapshotDto>(json).is_err());
    }

    /// Stored states written before v0.4 must still load.
    #[test]
    fn accepts_task_without_fields_added_later() {
        let json = serde_json::json!({
            "id": "01928f3a-0000-7000-8000-000000000001",
            "title": null,
            "status": null,
            "deleted": null,
        });

        let task = Task::try_from(serde_json::from_value::<TaskDto>(json).unwrap()).unwrap();

        assert_eq!(task.registers().priority, None);
        assert_eq!(task.registers().note, None);
        assert_eq!(task.registers().due, None);
        assert_eq!(task.registers().scheduled_at, None);
        assert_eq!(task.registers().parent, None);
        assert_eq!(task.registers().recurrence, None);
        assert!(task.registers().tags.is_empty());
    }

    #[test]
    fn tag_listed_twice_keeps_its_newer_register() {
        let tag = crate::tag::tests::work_tag().id();
        let mut dto = TaskDto::from(&partial_task());
        dto.tags = [(true, 5), (false, 9), (true, 7)]
            .map(|(tagged, wall)| TaskTagDto {
                tag_id: Uuid::from_u128(tag.0),
                tagged,
                hlc: HlcDto::from(at(wall)),
            })
            .to_vec();

        let task = Task::try_from(dto).unwrap();

        assert_eq!(task.registers().tags[&tag], Lww::new(false, at(9)));
    }

    #[test]
    fn rejects_non_canonical_note_register() {
        let mut dto = TaskDto::from(&full_task());
        dto.note = Some(RegisterDto {
            value: "oat milk ".to_owned(),
            hlc: HlcDto::from(at(1)),
        });

        assert_eq!(Task::try_from(dto), Err(ProtocolError::NonCanonicalNote));
    }

    #[test]
    fn rejects_non_canonical_title_register() {
        let mut dto = TaskDto::from(&full_task());
        dto.title = Some(RegisterDto {
            value: "buy milk ".to_owned(),
            hlc: HlcDto::from(at(1)),
        });

        assert_eq!(Task::try_from(dto), Err(ProtocolError::NonCanonicalTitle));
    }

    pub(crate) fn any_hlc() -> impl Strategy<Value = Hlc> {
        (any::<u64>(), any::<u32>(), any::<u128>())
            .prop_map(|(wall, counter, device)| Hlc::new(wall, counter, DeviceId(device)))
    }

    pub(crate) fn any_task() -> impl Strategy<Value = Task> {
        let change = prop_oneof![
            "[a-z]{1,8}".prop_map(|s| TaskChange::Title(title(&s))),
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
            "([a-z]{1,8}\n?){0,3}[a-z]".prop_map(|s| TaskChange::Note(Note::new(&s).unwrap())),
            prop::option::of(crate::date::tests::any_date()).prop_map(TaskChange::Due),
            prop::option::of(any::<u64>().prop_map(Timestamp)).prop_map(TaskChange::ScheduledAt),
            prop::option::of(any::<u128>().prop_map(TaskId)).prop_map(TaskChange::Parent),
            prop::option::of(crate::recurrence::tests::any_recurrence())
                .prop_map(TaskChange::Recurrence),
            (any::<u128>(), any::<bool>()).prop_map(|(tag, tagged)| TaskChange::Tag {
                tag: TagId(tag),
                tagged
            }),
        ];
        (
            any::<u128>(),
            prop::collection::vec((change, any_hlc()), 0..5),
        )
            .prop_map(|(id, writes)| {
                writes
                    .iter()
                    .fold(Task::empty(TaskId(id)), |task, (change, hlc)| {
                        task.apply(change, *hlc)
                    })
            })
    }

    pub(crate) fn any_entry() -> impl Strategy<Value = TimeEntry> {
        let time = any::<u64>().prop_map(Timestamp);
        let change = prop_oneof![
            time.clone().prop_map(TimeEntryChange::StartedAt),
            prop::option::of(time).prop_map(TimeEntryChange::StoppedAt),
            any::<bool>().prop_map(TimeEntryChange::Deleted),
        ];
        (
            any::<u128>(),
            any::<u128>(),
            prop::collection::vec((change, any_hlc()), 0..5),
        )
            .prop_map(|(id, task, writes)| {
                writes.iter().fold(
                    TimeEntry::empty(TimeEntryId(id), TaskId(task)),
                    |entry, (change, hlc)| entry.apply(change, *hlc),
                )
            })
    }

    proptest! {
        #[test]
        fn json_roundtrip_preserves_snapshot(
            tasks in prop::collection::vec(any_task(), 0..5),
            time_entries in prop::collection::vec(any_entry(), 0..5),
            tags in prop::collection::vec(crate::tag::tests::any_tag(), 0..5),
            log_id in any::<u128>().prop_map(LogId),
            seq in prop::option::of(any::<u64>().prop_map(Seq)),
        ) {
            let snapshot = Snapshot { log_id, tasks, time_entries, tags, seq };
            let json = serde_json::to_string(&SnapshotDto::from(&snapshot)).unwrap();
            let dto: SnapshotDto = serde_json::from_str(&json).unwrap();

            prop_assert_eq!(Snapshot::try_from(dto), Ok(snapshot));
        }
    }
}
