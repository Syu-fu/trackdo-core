//! Wire representation of operations.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use trackdo_core::hlc::{DeviceId, Hlc};
use trackdo_core::op::{Op, OpId};
use trackdo_core::task::{FieldChange, TaskId, TaskStatus};
use trackdo_core::title::{Title, TitleError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpDto {
    pub op_id: Uuid,
    pub task_id: Uuid,
    pub hlc: HlcDto,
    pub change: FieldChangeDto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HlcDto {
    pub wall_ms: u64,
    pub counter: u32,
    pub device: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "field", content = "value", rename_all = "snake_case")]
pub enum FieldChangeDto {
    Title(String),
    Status(TaskStatusDto),
    Deleted(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatusDto {
    Todo,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProtocolError {
    #[error("invalid title: {0}")]
    InvalidTitle(#[from] TitleError),
    /// Every replica must store the exact same value, so titles are not
    /// normalized on receipt.
    #[error("title is not in canonical form")]
    NonCanonicalTitle,
}

impl From<&Op> for OpDto {
    fn from(op: &Op) -> Self {
        Self {
            op_id: Uuid::from_u128(op.op_id.0),
            task_id: Uuid::from_u128(op.task_id.0),
            hlc: HlcDto::from(op.hlc),
            change: FieldChangeDto::from(&op.change),
        }
    }
}

impl TryFrom<OpDto> for Op {
    type Error = ProtocolError;

    fn try_from(dto: OpDto) -> Result<Self, Self::Error> {
        Ok(Self {
            op_id: OpId(dto.op_id.as_u128()),
            task_id: TaskId(dto.task_id.as_u128()),
            hlc: Hlc::from(dto.hlc),
            change: FieldChange::try_from(dto.change)?,
        })
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

impl From<&FieldChange> for FieldChangeDto {
    fn from(change: &FieldChange) -> Self {
        match change {
            FieldChange::Title(title) => Self::Title(title.as_str().to_owned()),
            FieldChange::Status(status) => Self::Status(TaskStatusDto::from(*status)),
            FieldChange::Deleted(deleted) => Self::Deleted(*deleted),
        }
    }
}

impl TryFrom<FieldChangeDto> for FieldChange {
    type Error = ProtocolError;

    fn try_from(dto: FieldChangeDto) -> Result<Self, Self::Error> {
        match dto {
            FieldChangeDto::Title(raw) => {
                let title = Title::new(&raw)?;
                if title.as_str() != raw {
                    return Err(ProtocolError::NonCanonicalTitle);
                }
                Ok(Self::Title(title))
            }
            FieldChangeDto::Status(status) => Ok(Self::Status(TaskStatus::from(status))),
            FieldChangeDto::Deleted(deleted) => Ok(Self::Deleted(deleted)),
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

    fn op(change: FieldChange) -> Op {
        Op {
            op_id: OpId(0x0192_8f3a_1b2c_7d4e_8f90_a1b2_c3d4_e5f6),
            task_id: TaskId(0x0192_8f3a_0000_7000_8000_0000_0000_0001),
            hlc: Hlc::new(
                1_730_000_000_000,
                3,
                DeviceId(0x0192_8f39_ffff_7abc_9def_0123_4567_89ab),
            ),
            change,
        }
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
            "task_id": "01928f3a-0000-7000-8000-000000000001",
            "hlc": {
                "wall_ms": 1_730_000_000_000u64,
                "counter": 3,
                "device": "01928f39-ffff-7abc-9def-0123456789ab",
            },
            "change": change,
        })
    }

    #[test]
    fn title_op_wire_format() {
        insta::assert_json_snapshot!(OpDto::from(&op(FieldChange::Title(title("buy milk")))));
    }

    #[test]
    fn status_op_wire_format() {
        insta::assert_json_snapshot!(OpDto::from(&op(FieldChange::Status(TaskStatus::Done))));
    }

    #[test]
    fn deleted_op_wire_format() {
        insta::assert_json_snapshot!(OpDto::from(&op(FieldChange::Deleted(true))));
    }

    #[test]
    fn decodes_wire_json() {
        let decoded = decode(wire(
            serde_json::json!({ "field": "status", "value": "todo" }),
        ));

        assert_eq!(decoded, Ok(op(FieldChange::Status(TaskStatus::Todo))));
    }

    #[test]
    fn ignores_unknown_fields_for_forward_compatibility() {
        let mut json = wire(serde_json::json!({ "field": "deleted", "value": false }));
        json["future_field"] = serde_json::json!(1);

        assert_eq!(decode(json), Ok(op(FieldChange::Deleted(false))));
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

    fn any_op() -> impl Strategy<Value = Op> {
        let change = prop_oneof![
            "[a-z]{1,8}( [a-z]{1,8})?".prop_map(|s| FieldChange::Title(title(&s))),
            prop::sample::select(vec![TaskStatus::Todo, TaskStatus::Done])
                .prop_map(FieldChange::Status),
            any::<bool>().prop_map(FieldChange::Deleted),
        ];
        (
            any::<u128>(),
            any::<u128>(),
            any::<u64>(),
            any::<u32>(),
            any::<u128>(),
            change,
        )
            .prop_map(|(op_id, task_id, wall, counter, device, change)| Op {
                op_id: OpId(op_id),
                task_id: TaskId(task_id),
                hlc: Hlc::new(wall, counter, DeviceId(device)),
                change,
            })
    }

    proptest! {
        #[test]
        fn json_roundtrip_preserves_op(op in any_op()) {
            prop_assert_eq!(roundtrip(&op), op);
        }
    }
}
