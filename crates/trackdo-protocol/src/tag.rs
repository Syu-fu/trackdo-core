//! Wire representation of tags and tag writes.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use trackdo_core::lww::Lww;
use trackdo_core::tag::{Tag, TagChange, TagColor, TagId, TagRegisters};

use crate::error::{ProtocolError, decode_tag_name};
use crate::op::HlcDto;
use crate::snapshot::RegisterDto;

/// A write to one tag field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "field", content = "value", rename_all = "snake_case")]
pub enum TagChangeDto {
    Name(String),
    Deleted(bool),
    Color(Option<TagColorDto>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TagColorDto {
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
    Pink,
    Gray,
}

/// A tag with every field's register, so receivers can merge it exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagDto {
    pub id: Uuid,
    pub name: Option<RegisterDto<String>>,
    pub deleted: Option<RegisterDto<bool>>,
    /// Missing in tags stored before colors existed.
    #[serde(default)]
    pub color: Option<RegisterDto<Option<TagColorDto>>>,
}

impl From<&TagChange> for TagChangeDto {
    fn from(change: &TagChange) -> Self {
        match change {
            TagChange::Name(name) => Self::Name(name.as_str().to_owned()),
            TagChange::Deleted(deleted) => Self::Deleted(*deleted),
            TagChange::Color(color) => Self::Color(color.map(TagColorDto::from)),
        }
    }
}

impl TryFrom<TagChangeDto> for TagChange {
    type Error = ProtocolError;

    fn try_from(dto: TagChangeDto) -> Result<Self, Self::Error> {
        match dto {
            TagChangeDto::Name(raw) => Ok(Self::Name(decode_tag_name(&raw)?)),
            TagChangeDto::Deleted(deleted) => Ok(Self::Deleted(deleted)),
            TagChangeDto::Color(color) => Ok(Self::Color(color.map(TagColor::from))),
        }
    }
}

impl From<TagColor> for TagColorDto {
    fn from(color: TagColor) -> Self {
        match color {
            TagColor::Red => Self::Red,
            TagColor::Orange => Self::Orange,
            TagColor::Yellow => Self::Yellow,
            TagColor::Green => Self::Green,
            TagColor::Blue => Self::Blue,
            TagColor::Purple => Self::Purple,
            TagColor::Pink => Self::Pink,
            TagColor::Gray => Self::Gray,
        }
    }
}

impl From<TagColorDto> for TagColor {
    fn from(dto: TagColorDto) -> Self {
        match dto {
            TagColorDto::Red => Self::Red,
            TagColorDto::Orange => Self::Orange,
            TagColorDto::Yellow => Self::Yellow,
            TagColorDto::Green => Self::Green,
            TagColorDto::Blue => Self::Blue,
            TagColorDto::Purple => Self::Purple,
            TagColorDto::Pink => Self::Pink,
            TagColorDto::Gray => Self::Gray,
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
            color: registers.color.as_ref().map(|reg| RegisterDto {
                value: reg.value().map(TagColorDto::from),
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
        let color = dto
            .color
            .map(|reg| Lww::new(reg.value.map(TagColor::from), reg.hlc.into()));
        Ok(Self::from_registers(
            TagId(dto.id.as_u128()),
            TagRegisters {
                name,
                deleted,
                color,
            },
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
    fn tag_color_op_wire_format() {
        insta::assert_json_snapshot!(OpDto::from(&op(TagChange::Color(Some(TagColor::Blue)))));
    }

    #[test]
    fn tag_color_removed_op_wire_format() {
        insta::assert_json_snapshot!(OpDto::from(&op(TagChange::Color(None))));
    }

    #[test]
    fn colored_tag_wire_format() {
        let tag = work_tag().apply(
            &TagChange::Color(Some(TagColor::Gray)),
            at(1_730_000_100_000),
        );

        insta::assert_json_snapshot!(TagDto::from(&tag));
    }

    #[test]
    fn every_color_has_a_distinct_wire_name() {
        let names: Vec<_> = TagColor::ALL
            .iter()
            .map(|&color| serde_json::to_value(TagColorDto::from(color)).unwrap())
            .collect();

        insta::assert_json_snapshot!(names);
    }

    /// Tags stored before colors existed must still load.
    #[test]
    fn accepts_tag_without_color() {
        let mut json = serde_json::to_value(TagDto::from(&work_tag())).unwrap();
        json.as_object_mut().unwrap().remove("color");
        let dto: TagDto = serde_json::from_value(json).unwrap();

        assert_eq!(Tag::try_from(dto), Ok(work_tag()));
    }

    #[test]
    fn rejects_unknown_color() {
        let json = serde_json::json!({ "field": "color", "value": "teal" });

        assert!(serde_json::from_value::<TagChangeDto>(json).is_err());
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

    fn any_color() -> impl Strategy<Value = Option<TagColor>> {
        prop::option::of(prop::sample::select(TagColor::ALL.to_vec()))
    }

    pub(crate) fn any_tag() -> impl Strategy<Value = Tag> {
        let change = prop_oneof![
            "[a-z]{1,8}".prop_map(|s| TagChange::Name(name(&s))),
            any::<bool>().prop_map(TagChange::Deleted),
            any_color().prop_map(TagChange::Color),
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
            any_color().prop_map(TagChange::Color),
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
