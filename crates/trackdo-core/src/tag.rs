//! Tags: named labels that tasks can carry.

use thiserror::Error;

use crate::hlc::Hlc;
use crate::lww::{Lww, join, write};
use crate::title::is_line_break;

/// Maximum tag name length, counted in Unicode scalar values.
pub const MAX_TAG_NAME_CHARS: usize = 50;

/// Namespace of the name-based UUIDs that [`TagId::for_name`] derives.
const TAG_NAMESPACE: uuid::Uuid = uuid::uuid!("ba0576af-8152-4ff7-bb1e-7ddf3cf284ec");

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TagId(pub u128);

impl TagId {
    /// The id a tag created as `name` gets, so devices that create the same
    /// tag while offline end up with one tag. A UUIDv5 of the name.
    pub fn for_name(name: &TagName) -> Self {
        Self(uuid::Uuid::new_v5(&TAG_NAMESPACE, name.as_str().as_bytes()).as_u128())
    }
}

/// A single-line, non-empty, trimmed name of at most
/// [`MAX_TAG_NAME_CHARS`]. Names are compared exactly, so `Work` and `work`
/// are different tags.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TagName(String);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TagNameError {
    #[error("tag name must not be empty")]
    Empty,
    #[error("tag name must be a single line")]
    ContainsLineBreak,
    #[error("tag name must be at most {max} characters, got {actual}")]
    TooLong { max: usize, actual: usize },
}

impl TagName {
    pub fn new(input: &str) -> Result<Self, TagNameError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(TagNameError::Empty);
        }
        if trimmed.chars().any(is_line_break) {
            return Err(TagNameError::ContainsLineBreak);
        }
        let len = trimmed.chars().count();
        if len > MAX_TAG_NAME_CHARS {
            return Err(TagNameError::TooLong {
                max: MAX_TAG_NAME_CHARS,
                actual: len,
            });
        }
        Ok(Self(trimmed.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A write to a single tag field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagChange {
    Name(TagName),
    Deleted(bool),
}

/// The register of every tag field, `None` until its first write arrives.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagRegisters {
    pub name: Option<Lww<TagName>>,
    pub deleted: Option<Lww<bool>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    id: TagId,
    registers: TagRegisters,
}

impl Tag {
    /// A tag that has not received any write yet.
    pub const fn empty(id: TagId) -> Self {
        Self {
            id,
            registers: TagRegisters {
                name: None,
                deleted: None,
            },
        }
    }

    /// Rebuilds a tag from previously exported registers.
    pub const fn from_registers(id: TagId, registers: TagRegisters) -> Self {
        Self { id, registers }
    }

    pub const fn id(&self) -> TagId {
        self.id
    }

    pub const fn registers(&self) -> &TagRegisters {
        &self.registers
    }

    /// `None` until a name write has been received.
    pub fn name(&self) -> Option<&TagName> {
        self.registers.name.as_ref().map(Lww::value)
    }

    pub fn is_deleted(&self) -> bool {
        self.registers
            .deleted
            .as_ref()
            .is_some_and(|deleted| *deleted.value())
    }

    /// Tag after applying `change` written at `hlc`. Stale writes are ignored.
    pub fn apply(&self, change: &TagChange, hlc: Hlc) -> Self {
        let current = &self.registers;
        let registers = match change {
            TagChange::Name(name) => TagRegisters {
                name: Some(write(&current.name, name.clone(), hlc)),
                ..current.clone()
            },
            TagChange::Deleted(deleted) => TagRegisters {
                deleted: Some(write(&current.deleted, *deleted, hlc)),
                ..current.clone()
            },
        };
        Self {
            id: self.id,
            registers,
        }
    }

    /// The write each field holds, as it would be sent again.
    pub(crate) fn writes(&self) -> impl Iterator<Item = (TagChange, Hlc)> {
        let TagRegisters { name, deleted } = &self.registers;
        let name = name
            .as_ref()
            .map(|name| (TagChange::Name(name.value().clone()), name.hlc()));
        let deleted = deleted
            .as_ref()
            .map(|deleted| (TagChange::Deleted(*deleted.value()), deleted.hlc()));
        [name, deleted].into_iter().flatten()
    }

    /// Timestamp of the most recent write to any field.
    pub(crate) fn newest_hlc(&self) -> Option<Hlc> {
        self.writes().map(|(_, hlc)| hlc).max()
    }

    /// Field-wise join with another copy of the same tag.
    pub(crate) fn merge(&self, other: &Self) -> Self {
        debug_assert_eq!(self.id, other.id, "merging different tags");
        let (a, b) = (&self.registers, &other.registers);
        Self {
            id: self.id,
            registers: TagRegisters {
                name: join(&a.name, &b.name),
                deleted: join(&a.deleted, &b.deleted),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hlc::DeviceId;
    use proptest::prelude::*;

    const ID: TagId = TagId(42);

    fn name(s: &str) -> TagName {
        TagName::new(s).unwrap()
    }

    fn at(wall: u64) -> Hlc {
        Hlc::new(wall, 0, DeviceId(1))
    }

    #[test]
    fn name_is_trimmed_single_line_and_bounded() {
        assert_eq!(name("  work ").as_str(), "work");
        assert_eq!(TagName::new(" "), Err(TagNameError::Empty));
        assert_eq!(TagName::new("a\nb"), Err(TagNameError::ContainsLineBreak));
        assert!(TagName::new(&"あ".repeat(MAX_TAG_NAME_CHARS)).is_ok());
        assert_eq!(
            TagName::new(&"a".repeat(MAX_TAG_NAME_CHARS + 1)),
            Err(TagNameError::TooLong {
                max: MAX_TAG_NAME_CHARS,
                actual: MAX_TAG_NAME_CHARS + 1
            })
        );
    }

    #[test]
    fn id_for_name_is_a_stable_uuid_v5() {
        let id = TagId::for_name(&name("work"));

        assert_eq!(id, TagId::for_name(&name(" work")));
        assert_ne!(id, TagId::for_name(&name("Work")));
        assert_eq!(uuid::Uuid::from_u128(id.0).get_version_num(), 5);
        // Pinned so that every release derives the same id.
        assert_eq!(
            uuid::Uuid::from_u128(id.0).to_string(),
            "dc59ba82-d0ee-5fec-b8e2-c6935ec3ee03"
        );
    }

    #[test]
    fn apply_sets_fields_and_ignores_stale_writes() {
        let tag = Tag::empty(ID)
            .apply(&TagChange::Name(name("new")), at(2))
            .apply(&TagChange::Name(name("old")), at(1))
            .apply(&TagChange::Deleted(true), at(3));

        assert_eq!(tag.name(), Some(&name("new")));
        assert!(tag.is_deleted());
        assert_eq!(tag.newest_hlc(), Some(at(3)));
        assert_eq!(
            tag.writes().collect::<Vec<_>>(),
            vec![
                (TagChange::Name(name("new")), at(2)),
                (TagChange::Deleted(true), at(3)),
            ]
        );
    }

    fn write_strategy() -> impl Strategy<Value = (TagChange, Hlc)> {
        let change = prop_oneof![
            prop::sample::select(vec!["a", "b"]).prop_map(|s| TagChange::Name(name(s))),
            any::<bool>().prop_map(TagChange::Deleted),
        ];
        (change, 0u64..4, 0u128..3)
            .prop_map(|(change, wall, device)| (change, Hlc::new(wall, 0, DeviceId(device))))
    }

    fn apply_all(writes: &[(TagChange, Hlc)]) -> Tag {
        writes
            .iter()
            .fold(Tag::empty(ID), |tag, (change, hlc)| tag.apply(change, *hlc))
    }

    proptest! {
        #[test]
        fn merging_states_equals_applying_all_writes(
            left in prop::collection::vec(write_strategy(), 0..8),
            right in prop::collection::vec(write_strategy(), 0..8),
        ) {
            let both: Vec<_> = left.iter().chain(right.iter()).cloned().collect();

            prop_assert_eq!(apply_all(&left).merge(&apply_all(&right)), apply_all(&both));
            prop_assert_eq!(apply_all(&right).merge(&apply_all(&left)), apply_all(&both));
        }

        #[test]
        fn rewriting_its_own_writes_changes_nothing(
            writes in prop::collection::vec(write_strategy(), 0..8),
        ) {
            let tag = apply_all(&writes);
            let again: Vec<_> = tag.writes().collect();

            prop_assert_eq!(apply_all(&again), tag.clone());
            prop_assert_eq!(again.iter().fold(tag.clone(), |t, (c, h)| t.apply(c, *h)), tag);
        }
    }
}
