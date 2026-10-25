//! Commands on and queries of tags.

use super::{CommandError, Replica};
use crate::env::{Clock, IdGen};
use crate::op::OpTarget;
use crate::tag::{Tag, TagChange, TagId, TagName};

impl<C: Clock, G: IdGen> Replica<C, G> {
    /// Creates a tag named `name` and returns its id, or returns the id of
    /// the tag already named so.
    ///
    /// A new tag takes the id derived from its name, so devices creating the
    /// same tag offline get one tag. Should a renamed or deleted tag hold
    /// that id, the new tag takes a fresh id instead.
    pub fn create_tag(&mut self, name: &str) -> Result<TagId, CommandError> {
        let name = TagName::new(name)?;
        if let Some(existing) = self.live_tag_named(&name) {
            return Ok(existing);
        }
        let derived = TagId::for_name(&name);
        let id = if self.tags.contains_key(&derived) {
            self.ids.tag_id()
        } else {
            derived
        };
        self.emit_tag(id, TagChange::Name(name))?;
        Ok(id)
    }

    /// Fails with [`CommandError::TagNameTaken`] when another tag has the
    /// name.
    pub fn rename_tag(&mut self, id: TagId, name: &str) -> Result<(), CommandError> {
        self.ensure_tag_editable(id)?;
        let name = TagName::new(name)?;
        if let Some(other) = self.live_tag_named(&name).filter(|&other| other != id) {
            return Err(CommandError::TagNameTaken(other));
        }
        self.emit_tag(id, TagChange::Name(name))
    }

    /// The tag stays in storage as a tombstone.
    pub fn delete_tag(&mut self, id: TagId) -> Result<(), CommandError> {
        self.ensure_tag_editable(id)?;
        self.emit_tag(id, TagChange::Deleted(true))
    }

    pub fn tag(&self, id: TagId) -> Option<&Tag> {
        self.tags.get(&id)
    }

    /// All known tags, including deleted ones, ordered by id.
    pub fn tags(&self) -> impl Iterator<Item = &Tag> {
        self.tags.values()
    }

    fn live_tag_named(&self, name: &TagName) -> Option<TagId> {
        self.tags
            .values()
            .find(|tag| !tag.is_deleted() && tag.name() == Some(name))
            .map(Tag::id)
    }

    pub(super) fn ensure_tag_editable(&self, id: TagId) -> Result<(), CommandError> {
        match self.tags.get(&id) {
            None => Err(CommandError::TagNotFound(id)),
            Some(tag) if tag.is_deleted() => Err(CommandError::TagDeleted(id)),
            Some(_) => Ok(()),
        }
    }

    fn emit_tag(&mut self, tag_id: TagId, change: TagChange) -> Result<(), CommandError> {
        self.emit(vec![OpTarget::Tag { tag_id, change }])
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use crate::hlc::Hlc;
    use crate::op::{Op, OpId, Seq};
    use crate::tag::TagNameError;

    fn tag_name(s: &str) -> TagName {
        TagName::new(s).unwrap()
    }

    fn tag_changes(replica: &TestReplica) -> Vec<(TagId, TagChange)> {
        replica
            .pending_ops()
            .iter()
            .filter_map(|op| match &op.target {
                OpTarget::Tag { tag_id, change } => Some((*tag_id, change.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn create_tag_derives_its_id_from_the_name() {
        let mut replica = replica();

        let id = replica.create_tag(" work ").unwrap();

        assert_eq!(id, TagId::for_name(&tag_name("work")));
        assert_eq!(replica.tag(id).unwrap().name(), Some(&tag_name("work")));
        assert_eq!(
            tag_changes(&replica),
            vec![(id, TagChange::Name(tag_name("work")))]
        );
    }

    #[test]
    fn create_tag_returns_the_tag_already_named_so() {
        let mut replica = replica();
        let id = replica.create_tag("work").unwrap();
        let renamed = replica.create_tag("home").unwrap();
        replica.rename_tag(renamed, "chores").unwrap();
        let pending_before = replica.pending_ops().len();

        assert_eq!(replica.create_tag("work"), Ok(id));
        assert_eq!(replica.create_tag("chores"), Ok(renamed));
        assert_eq!(replica.pending_ops().len(), pending_before);
    }

    #[test]
    fn create_tag_takes_a_fresh_id_when_its_name_id_is_in_use() {
        let mut replica = replica();
        let renamed = replica.create_tag("work").unwrap();
        replica.rename_tag(renamed, "job").unwrap();
        let deleted = replica.create_tag("home").unwrap();
        replica.delete_tag(deleted).unwrap();

        let work = replica.create_tag("work").unwrap();
        let home = replica.create_tag("home").unwrap();

        assert_ne!(work, renamed);
        assert_ne!(home, deleted);
        assert_eq!(replica.tag(work).unwrap().name(), Some(&tag_name("work")));
        assert!(!replica.tag(home).unwrap().is_deleted());
    }

    #[test]
    fn create_tag_rejects_invalid_name() {
        let mut replica = replica();

        assert_eq!(
            replica.create_tag(" "),
            Err(CommandError::TagName(TagNameError::Empty))
        );
        assert!(replica.pending_ops().is_empty());
    }

    #[test]
    fn rename_tag_renames_and_refuses_names_in_use() {
        let mut replica = replica();
        let work = replica.create_tag("work").unwrap();
        let home = replica.create_tag("home").unwrap();

        replica.rename_tag(work, "job").unwrap();
        replica.rename_tag(work, "job").unwrap();

        assert_eq!(replica.tag(work).unwrap().name(), Some(&tag_name("job")));
        assert_eq!(
            replica.rename_tag(home, "job"),
            Err(CommandError::TagNameTaken(work))
        );
    }

    #[test]
    fn deleted_tag_frees_its_name() {
        let mut replica = replica();
        let work = replica.create_tag("work").unwrap();
        let home = replica.create_tag("home").unwrap();

        replica.delete_tag(work).unwrap();

        assert!(replica.tag(work).unwrap().is_deleted());
        assert_eq!(replica.rename_tag(home, "work"), Ok(()));
    }

    #[test]
    fn commands_on_unknown_or_deleted_tags_fail() {
        let mut replica = replica();
        let gone = replica.create_tag("gone").unwrap();
        replica.delete_tag(gone).unwrap();
        let unknown = TagId(999);

        assert_eq!(
            replica.rename_tag(unknown, "a"),
            Err(CommandError::TagNotFound(unknown))
        );
        assert_eq!(
            replica.delete_tag(unknown),
            Err(CommandError::TagNotFound(unknown))
        );
        assert_eq!(
            replica.rename_tag(gone, "a"),
            Err(CommandError::TagDeleted(gone))
        );
        assert_eq!(
            replica.delete_tag(gone),
            Err(CommandError::TagDeleted(gone))
        );
    }

    #[test]
    fn tags_lists_every_tag_by_id() {
        let mut replica = replica();
        let a = replica.create_tag("a").unwrap();
        let b = replica.create_tag("b").unwrap();
        replica.delete_tag(b).unwrap();

        let mut expected = vec![a, b];
        expected.sort();
        assert_eq!(replica.tags().map(Tag::id).collect::<Vec<_>>(), expected);
    }

    #[test]
    fn remote_tag_writes_apply() {
        let mut replica = replica();
        let id = TagId(77);
        let op = Op::tag(
            OpId(900),
            id,
            Hlc::new(NOW + 5, 0, OTHER),
            TagChange::Name(tag_name("remote")),
        );

        replica.apply_remote(&op, Seq(1)).unwrap();

        assert_eq!(replica.tag(id).unwrap().name(), Some(&tag_name("remote")));
        assert!(replica.hlc.wall_ms() >= NOW + 5);
    }

    #[test]
    fn state_round_trips_tags() {
        let mut replica = replica();
        let id = replica.create_tag("work").unwrap();

        let restored = TestReplica::restore(replica.state(), FixedClock(NOW), SequentialIds(100));

        assert_eq!(restored.tag(id), replica.tag(id));
    }
}
