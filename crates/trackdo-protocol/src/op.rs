//! Wire representation of operations.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use trackdo_core::hlc::{DeviceId, Hlc};
use trackdo_core::op::{Op, OpId, OpTarget};
use trackdo_core::task::{TaskChange, TaskId, TaskStatus};
use trackdo_core::time_entry::{TimeEntryChange, TimeEntryId, Timestamp};

use crate::error::{ProtocolError, decode_title};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpDto {
    pub op_id: Uuid,
    pub hlc: HlcDto,
    pub target: OpTargetDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OpTargetDto {
    Task {
        task_id: Uuid,
        change: TaskChangeDto,
    },
    TimeEntry {
        entry_id: Uuid,
        task_id: Uuid,
        change: TimeEntryChangeDto,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HlcDto {
    pub wall_ms: u64,
    pub counter: u32,
    pub device: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "field", content = "value", rename_all = "snake_case")]
pub enum TaskChangeDto {
    Title(String),
    Status(TaskStatusDto),
    Deleted(bool),
}

/// Times are milliseconds since the Unix epoch, in UTC. `stopped_at: null`
/// means the timer is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "field", content = "value", rename_all = "snake_case")]
pub enum TimeEntryChangeDto {
    StartedAt(u64),
    StoppedAt(Option<u64>),
    Deleted(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatusDto {
    Todo,
    Done,
}

impl From<&Op> for OpDto {
    fn from(op: &Op) -> Self {
        Self {
            op_id: Uuid::from_u128(op.op_id.0),
            hlc: HlcDto::from(op.hlc),
            target: OpTargetDto::from(&op.target),
        }
    }
}

impl TryFrom<OpDto> for Op {
    type Error = ProtocolError;

    fn try_from(dto: OpDto) -> Result<Self, Self::Error> {
        Ok(Self {
            op_id: OpId(dto.op_id.as_u128()),
            hlc: Hlc::from(dto.hlc),
            target: OpTarget::try_from(dto.target)?,
        })
    }
}

impl From<&OpTarget> for OpTargetDto {
    fn from(target: &OpTarget) -> Self {
        match target {
            OpTarget::Task { task_id, change } => Self::Task {
                task_id: Uuid::from_u128(task_id.0),
                change: TaskChangeDto::from(change),
            },
            OpTarget::TimeEntry {
                entry_id,
                task_id,
                change,
            } => Self::TimeEntry {
                entry_id: Uuid::from_u128(entry_id.0),
                task_id: Uuid::from_u128(task_id.0),
                change: TimeEntryChangeDto::from(change),
            },
        }
    }
}

impl TryFrom<OpTargetDto> for OpTarget {
    type Error = ProtocolError;

    fn try_from(dto: OpTargetDto) -> Result<Self, Self::Error> {
        match dto {
            OpTargetDto::Task { task_id, change } => Ok(Self::Task {
                task_id: TaskId(task_id.as_u128()),
                change: TaskChange::try_from(change)?,
            }),
            OpTargetDto::TimeEntry {
                entry_id,
                task_id,
                change,
            } => Ok(Self::TimeEntry {
                entry_id: TimeEntryId(entry_id.as_u128()),
                task_id: TaskId(task_id.as_u128()),
                change: TimeEntryChange::from(change),
            }),
        }
    }
}

impl From<Hlc> for HlcDto {
    fn from(hlc: Hlc) -> Self {
        Self {
            wall_ms: hlc.wall_ms(),
            counter: hlc.counter(),
            device: Uuid::from_u128(hlc.device().0),
        }
    }
}

impl From<HlcDto> for Hlc {
    fn from(dto: HlcDto) -> Self {
        Self::new(dto.wall_ms, dto.counter, DeviceId(dto.device.as_u128()))
    }
}

impl From<&TaskChange> for TaskChangeDto {
    fn from(change: &TaskChange) -> Self {
        match change {
            TaskChange::Title(title) => Self::Title(title.as_str().to_owned()),
            TaskChange::Status(status) => Self::Status(TaskStatusDto::from(*status)),
            TaskChange::Deleted(deleted) => Self::Deleted(*deleted),
        }
    }
}

impl TryFrom<TaskChangeDto> for TaskChange {
    type Error = ProtocolError;

    fn try_from(dto: TaskChangeDto) -> Result<Self, Self::Error> {
        match dto {
            TaskChangeDto::Title(raw) => Ok(Self::Title(decode_title(&raw)?)),
            TaskChangeDto::Status(status) => Ok(Self::Status(TaskStatus::from(status))),
            TaskChangeDto::Deleted(deleted) => Ok(Self::Deleted(deleted)),
        }
    }
}

impl From<&TimeEntryChange> for TimeEntryChangeDto {
    fn from(change: &TimeEntryChange) -> Self {
        match change {
            TimeEntryChange::StartedAt(Timestamp(started)) => Self::StartedAt(*started),
            TimeEntryChange::StoppedAt(stopped) => {
                Self::StoppedAt(stopped.map(|Timestamp(stopped)| stopped))
            }
            TimeEntryChange::Deleted(deleted) => Self::Deleted(*deleted),
        }
    }
}

impl From<TimeEntryChangeDto> for TimeEntryChange {
    fn from(dto: TimeEntryChangeDto) -> Self {
        match dto {
            TimeEntryChangeDto::StartedAt(started) => Self::StartedAt(Timestamp(started)),
            TimeEntryChangeDto::StoppedAt(stopped) => Self::StoppedAt(stopped.map(Timestamp)),
            TimeEntryChangeDto::Deleted(deleted) => Self::Deleted(deleted),
        }
    }
}

impl From<TaskStatus> for TaskStatusDto {
    fn from(status: TaskStatus) -> Self {
        match status {
            TaskStatus::Todo => Self::Todo,
            TaskStatus::Done => Self::Done,
        }
    }
}

impl From<TaskStatusDto> for TaskStatus {
    fn from(dto: TaskStatusDto) -> Self {
        match dto {
            TaskStatusDto::Todo => Self::Todo,
            TaskStatusDto::Done => Self::Done,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use trackdo_core::title::{Title, TitleError};

    fn op(change: TaskChange) -> Op {
        Op::task(
            OpId(0x0192_8f3a_1b2c_7d4e_8f90_a1b2_c3d4_e5f6),
            TaskId(0x0192_8f3a_0000_7000_8000_0000_0000_0001),
            Hlc::new(
                1_730_000_000_000,
                3,
                DeviceId(0x0192_8f39_ffff_7abc_9def_0123_4567_89ab),
            ),
            change,
        )
    }

    fn title(s: &str) -> Title {
        Title::new(s).unwrap()
    }

    fn roundtrip(op: &Op) -> Op {
        let json = serde_json::to_string(&OpDto::from(op)).unwrap();
        let dto: OpDto = serde_json::from_str(&json).unwrap();
        Op::try_from(dto).unwrap()
    }

    fn decode(json: serde_json::Value) -> Result<Op, ProtocolError> {
        Op::try_from(serde_json::from_value::<OpDto>(json).unwrap())
    }

    fn wire(change: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "op_id": "01928f3a-1b2c-7d4e-8f90-a1b2c3d4e5f6",
            "hlc": {
                "wall_ms": 1_730_000_000_000u64,
                "counter": 3,
                "device": "01928f39-ffff-7abc-9def-0123456789ab",
            },
            "target": {
                "type": "task",
                "task_id": "01928f3a-0000-7000-8000-000000000001",
                "change": change,
            },
        })
    }

    #[test]
    fn title_op_wire_format() {
        insta::assert_json_snapshot!(OpDto::from(&op(TaskChange::Title(title("buy milk")))));
    }

    #[test]
    fn status_op_wire_format() {
        insta::assert_json_snapshot!(OpDto::from(&op(TaskChange::Status(TaskStatus::Done))));
    }

    #[test]
    fn deleted_op_wire_format() {
        insta::assert_json_snapshot!(OpDto::from(&op(TaskChange::Deleted(true))));
    }

    const ENTRY: TimeEntryId = TimeEntryId(0x0192_8f3a_0000_7000_8000_0000_0000_0e01);

    fn entry_op(change: TimeEntryChange) -> Op {
        let task = op(TaskChange::Deleted(false));
        Op::time_entry(
            task.op_id,
            ENTRY,
            TaskId(0x0192_8f3a_0000_7000_8000_0000_0000_0001),
            task.hlc,
            change,
        )
    }

    fn entry_wire(change: serde_json::Value) -> serde_json::Value {
        let mut json = wire(serde_json::Value::Null);
        json["target"] = serde_json::json!({
            "type": "time_entry",
            "entry_id": "01928f3a-0000-7000-8000-000000000e01",
            "task_id": "01928f3a-0000-7000-8000-000000000001",
            "change": change,
        });
        json
    }

    #[test]
    fn started_at_op_wire_format() {
        insta::assert_json_snapshot!(OpDto::from(&entry_op(TimeEntryChange::StartedAt(
            Timestamp(1_730_000_000_000)
        ))));
    }

    #[test]
    fn stopped_at_op_wire_format() {
        insta::assert_json_snapshot!(OpDto::from(&entry_op(TimeEntryChange::StoppedAt(Some(
            Timestamp(1_730_000_360_000)
        )))));
    }

    #[test]
    fn running_op_wire_format() {
        insta::assert_json_snapshot!(OpDto::from(&entry_op(TimeEntryChange::StoppedAt(None))));
    }

    #[test]
    fn entry_deleted_op_wire_format() {
        insta::assert_json_snapshot!(OpDto::from(&entry_op(TimeEntryChange::Deleted(true))));
    }

    #[test]
    fn decodes_running_entry_from_null_stop() {
        let json = entry_wire(serde_json::json!({ "field": "stopped_at", "value": null }));

        assert_eq!(decode(json), Ok(entry_op(TimeEntryChange::StoppedAt(None))));
    }

    #[test]
    fn rejects_entry_op_without_task() {
        let mut json = entry_wire(serde_json::json!({ "field": "deleted", "value": true }));
        json["target"].as_object_mut().unwrap().remove("task_id");

        assert!(serde_json::from_value::<OpDto>(json).is_err());
    }

    #[test]
    fn decodes_wire_json() {
        let decoded = decode(wire(
            serde_json::json!({ "field": "status", "value": "todo" }),
        ));

        assert_eq!(decoded, Ok(op(TaskChange::Status(TaskStatus::Todo))));
    }

    #[test]
    fn ignores_unknown_fields_for_forward_compatibility() {
        let mut json = wire(serde_json::json!({ "field": "deleted", "value": false }));
        json["future_field"] = serde_json::json!(1);

        assert_eq!(decode(json), Ok(op(TaskChange::Deleted(false))));
    }

    #[test]
    fn rejects_invalid_title() {
        let json = wire(serde_json::json!({ "field": "title", "value": "a\nb" }));

        assert_eq!(
            decode(json),
            Err(ProtocolError::InvalidTitle(TitleError::ContainsLineBreak))
        );
    }

    #[test]
    fn rejects_non_canonical_title() {
        let json = wire(serde_json::json!({ "field": "title", "value": " padded " }));

        assert_eq!(decode(json), Err(ProtocolError::NonCanonicalTitle));
    }

    #[test]
    fn rejects_unknown_status() {
        let json = wire(serde_json::json!({ "field": "status", "value": "doing" }));

        assert!(serde_json::from_value::<OpDto>(json).is_err());
    }

    fn any_entry_op() -> impl Strategy<Value = Op> {
        let time = any::<u64>().prop_map(Timestamp);
        let change = prop_oneof![
            time.clone().prop_map(TimeEntryChange::StartedAt),
            prop::option::of(time).prop_map(TimeEntryChange::StoppedAt),
            any::<bool>().prop_map(TimeEntryChange::Deleted),
        ];
        (
            any::<u128>(),
            any::<u128>(),
            any::<u128>(),
            any::<u64>(),
            any::<u32>(),
            any::<u128>(),
            change,
        )
            .prop_map(
                |(op_id, entry_id, task_id, wall, counter, device, change)| {
                    Op::time_entry(
                        OpId(op_id),
                        TimeEntryId(entry_id),
                        TaskId(task_id),
                        Hlc::new(wall, counter, DeviceId(device)),
                        change,
                    )
                },
            )
    }

    fn any_task_op() -> impl Strategy<Value = Op> {
        let change = prop_oneof![
            "[a-z]{1,8}( [a-z]{1,8})?".prop_map(|s| TaskChange::Title(title(&s))),
            prop::sample::select(vec![TaskStatus::Todo, TaskStatus::Done])
                .prop_map(TaskChange::Status),
            any::<bool>().prop_map(TaskChange::Deleted),
        ];
        (
            any::<u128>(),
            any::<u128>(),
            any::<u64>(),
            any::<u32>(),
            any::<u128>(),
            change,
        )
            .prop_map(|(op_id, task_id, wall, counter, device, change)| {
                Op::task(
                    OpId(op_id),
                    TaskId(task_id),
                    Hlc::new(wall, counter, DeviceId(device)),
                    change,
                )
            })
    }

    proptest! {
        #[test]
        fn json_roundtrip_preserves_op(op in prop_oneof![any_task_op(), any_entry_op()]) {
            prop_assert_eq!(roundtrip(&op), op);
        }
    }
}
