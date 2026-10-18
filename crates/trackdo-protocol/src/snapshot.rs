//! Wire representation of a full state snapshot (`GET /snapshot`).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use trackdo_core::lww::Lww;
use trackdo_core::op::{LogId, Seq};
use trackdo_core::replica::Snapshot;
use trackdo_core::task::{Task, TaskId, TaskStatus};
use trackdo_core::time_entry::{TimeEntry, TimeEntryId, Timestamp};

use crate::error::{ProtocolError, decode_title};
use crate::op::{HlcDto, TaskStatusDto};

/// All tasks and time entries of the log `log_id` as of `seq`. `seq` is
/// `null` when the log is still empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotDto {
    pub log_id: Uuid,
    pub seq: Option<u64>,
    pub tasks: Vec<TaskDto>,
    pub time_entries: Vec<TimeEntryDto>,
}

/// A task with every field's register, so receivers can merge it exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDto {
    pub id: Uuid,
    pub title: Option<RegisterDto<String>>,
    pub status: Option<RegisterDto<TaskStatusDto>>,
    pub deleted: Option<RegisterDto<bool>>,
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
            seq: dto.seq.map(Seq),
        })
    }
}

impl From<&Task> for TaskDto {
    fn from(task: &Task) -> Self {
        Self {
            id: Uuid::from_u128(task.id().0),
            title: task
                .title_register()
                .map(|reg| encode(reg, |title| title.as_str().to_owned())),
            status: task
                .status_register()
                .map(|reg| encode(reg, |status| TaskStatusDto::from(*status))),
            deleted: task
                .deleted_register()
                .map(|reg| encode(reg, |deleted| *deleted)),
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
        let status = dto.status.map(|reg| decode(reg, TaskStatus::from));
        let deleted = dto.deleted.map(|reg| decode(reg, |deleted| deleted));
        Ok(Self::from_registers(
            TaskId(dto.id.as_u128()),
            title,
            status,
            deleted,
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
    use trackdo_core::hlc::{DeviceId, Hlc};
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
    }

    fn partial_task() -> Task {
        Task::empty(TaskId(0x0192_8f3a_0000_7000_8000_0000_0000_0002))
            .apply(&TaskChange::Status(TaskStatus::Todo), at(1_730_000_001_000))
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
            seq: Some(Seq(42)),
        }));
    }

    #[test]
    fn empty_snapshot_wire_format() {
        insta::assert_json_snapshot!(SnapshotDto::from(&Snapshot {
            log_id: LOG,
            tasks: Vec::new(),
            time_entries: Vec::new(),
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
    fn rejects_snapshot_without_time_entries() {
        let json = serde_json::json!({ "log_id": LOG_UUID, "seq": null, "tasks": [] });

        assert!(serde_json::from_value::<SnapshotDto>(json).is_err());
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
            log_id in any::<u128>().prop_map(LogId),
            seq in prop::option::of(any::<u64>().prop_map(Seq)),
        ) {
            let snapshot = Snapshot { log_id, tasks, time_entries, seq };
            let json = serde_json::to_string(&SnapshotDto::from(&snapshot)).unwrap();
            let dto: SnapshotDto = serde_json::from_str(&json).unwrap();

            prop_assert_eq!(Snapshot::try_from(dto), Ok(snapshot));
        }
    }
}
