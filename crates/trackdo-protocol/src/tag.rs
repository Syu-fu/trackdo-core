//! Wire representation of tags and tag writes.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use trackdo_core::lww::Lww;
use trackdo_core::tag::{Tag, TagChange, TagId, TagRegisters};

use crate::error::{ProtocolError, decode_tag_name};
use crate::op::HlcDto;
use crate::snapshot::RegisterDto;

/// A write to one tag field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "field", content = "value", rename_all = "snake_case")]
pub enum TagChangeDto {
    Name(String),
    Deleted(bool),
}

/// A tag with every field's register, so receivers can merge it exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagDto {
    pub id: Uuid,
    pub name: Option<RegisterDto<String>>,
    pub deleted: Option<RegisterDto<bool>>,
}

impl From<&TagChange> for TagChangeDto {
    fn from(change: &TagChange) -> Self {
        match change {
            TagChange::Name(name) => Self::Name(name.as_str().to_owned()),
            TagChange::Deleted(deleted) => Self::Deleted(*deleted),
        }
    }
}

impl TryFrom<TagChangeDto> for TagChange {
    type Error = ProtocolError;

    fn try_from(dto: TagChangeDto) -> Result<Self, Self::Error> {
        match dto {
            TagChangeDto::Name(raw) => Ok(Self::Name(decode_tag_name(&raw)?)),
            TagChangeDto::Deleted(deleted) => Ok(Self::Deleted(deleted)),
        }
    }
}

impl From<&Tag> for TagDto {
    fn from(tag: &Tag) -> Self {
        let registers = tag.registers();
        Self {
            id: Uuid::from_u128(tag.id().0),
            name: registers.name.as_ref().map(|reg| RegisterDto {
                value: reg.value().as_str().to_owned(),
                hlc: HlcDto::from(reg.hlc()),
            }),
            deleted: registers.deleted.as_ref().map(|reg| RegisterDto {
                value: *reg.value(),
                hlc: HlcDto::from(reg.hlc()),
            }),
        }
    }
}

impl TryFrom<TagDto> for Tag {
    type Error = ProtocolError;

    fn try_from(dto: TagDto) -> Result<Self, Self::Error> {
        let name = dto
            .name
            .map(|reg| {
                Ok::<_, ProtocolError>(Lww::new(decode_tag_name(&reg.value)?, reg.hlc.into()))
            })
            .transpose()?;
        let deleted = dto.deleted.map(|reg| Lww::new(reg.value, reg.hlc.into()));
        Ok(Self::from_registers(
            TagId(dto.id.as_u128()),
            TagRegisters { name, deleted },
        ))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use proptest::prelude::*;
    use trackdo_core::hlc::{DeviceId, Hlc};
    use trackdo_core::op::{Op, OpId};
    use trackdo_core::tag::TagName;

    use crate::op::OpDto;

    const DEVICE: DeviceId = DeviceId(0x0192_8f39_ffff_7abc_9def_0123_4567_89ab);
    const TAG: TagId = TagId(0xdc59_ba82_d0ee_5fec_b8e2_c693_5ec3_ee03);

    fn at(wall: u64) -> Hlc {
        Hlc::new(wall, 0, DEVICE)
    }

    fn name(s: &str) -> TagName {
        TagName::new(s).unwrap()
    }

    fn op(change: TagChange) -> Op {
        Op::tag(
            OpId(0x0192_8f3a_1b2c_7d4e_8f90_a1b2_c3d4_e5f6),
            TAG,
            at(1_730_000_000_000),
            change,
        )
    }

    pub(crate) fn work_tag() -> Tag {
        Tag::empty(TAG).apply(&TagChange::Name(name("work")), at(1_730_000_000_000))
    }

    #[test]
    fn tag_name_op_wire_format() {
        insta::assert_json_snapshot!(OpDto::from(&op(TagChange::Name(name("work")))));
    }

    #[test]
    fn tag_deleted_op_wire_format() {
        insta::assert_json_snapshot!(OpDto::from(&op(TagChange::Deleted(true))));
    }

    #[test]
    fn rejects_non_canonical_tag_name_in_op() {
        let json = serde_json::json!({ "field": "name", "value": " work" });
        let dto: TagChangeDto = serde_json::from_value(json).unwrap();

        assert_eq!(
            TagChange::try_from(dto),
            Err(ProtocolError::NonCanonicalTagName)
        );
    }

    #[test]
    fn rejects_invalid_tag_name() {
        let dto = TagChangeDto::Name(String::new());

        assert!(matches!(
            TagChange::try_from(dto),
            Err(ProtocolError::InvalidTagName(_))
        ));
    }

    #[test]
    fn rejects_non_canonical_tag_name_register() {
        let mut dto = TagDto::from(&work_tag());
        dto.name = Some(RegisterDto {
            value: "work ".to_owned(),
            hlc: HlcDto::from(at(1)),
        });

        assert_eq!(Tag::try_from(dto), Err(ProtocolError::NonCanonicalTagName));
    }

    pub(crate) fn any_tag() -> impl Strategy<Value = Tag> {
        let change = prop_oneof![
            "[a-z]{1,8}".prop_map(|s| TagChange::Name(name(&s))),
            any::<bool>().prop_map(TagChange::Deleted),
        ];
        let write = (change, any::<u64>(), any::<u128>())
            .prop_map(|(change, wall, device)| (change, Hlc::new(wall, 0, DeviceId(device))));
        (any::<u128>(), prop::collection::vec(write, 0..4)).prop_map(|(id, writes)| {
            writes
                .iter()
                .fold(Tag::empty(TagId(id)), |tag, (change, hlc)| {
                    tag.apply(change, *hlc)
                })
        })
    }

    pub(crate) fn any_tag_op() -> impl Strategy<Value = Op> {
        let change = prop_oneof![
            "[a-z]{1,8}( [a-z]{1,8})?".prop_map(|s| TagChange::Name(name(&s))),
            any::<bool>().prop_map(TagChange::Deleted),
        ];
        (
            any::<u128>(),
            any::<u128>(),
            any::<u64>(),
            any::<u128>(),
            change,
        )
            .prop_map(|(op_id, tag_id, wall, device, change)| {
                Op::tag(
                    OpId(op_id),
                    TagId(tag_id),
                    Hlc::new(wall, 0, DeviceId(device)),
                    change,
                )
            })
    }

    proptest! {
        #[test]
        fn json_roundtrip_preserves_tag(tag in any_tag()) {
            let json = serde_json::to_string(&TagDto::from(&tag)).unwrap();
            let dto: TagDto = serde_json::from_str(&json).unwrap();

            prop_assert_eq!(Tag::try_from(dto), Ok(tag));
        }

        #[test]
        fn json_roundtrip_preserves_tag_op(op in any_tag_op()) {
            let json = serde_json::to_string(&OpDto::from(&op)).unwrap();
            let dto: OpDto = serde_json::from_str(&json).unwrap();

            prop_assert_eq!(Op::try_from(dto), Ok(op));
        }
    }
}
