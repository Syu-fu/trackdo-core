//! Environment capabilities injected by the host, keeping the core free of I/O.

use crate::op::OpId;
use crate::tag::TagId;
use crate::task::TaskId;
use crate::time_entry::TimeEntryId;

/// Source of physical time.
pub trait Clock {
    /// Milliseconds since the Unix epoch. May go backwards; the HLC absorbs it.
    fn now_millis(&self) -> u64;
}

/// Source of globally unique identifiers, typically UUIDv7.
pub trait IdGen {
    fn task_id(&mut self) -> TaskId;
    fn time_entry_id(&mut self) -> TimeEntryId;
    /// For a tag whose name-derived id is taken; see
    /// [`TagId::for_name`].
    fn tag_id(&mut self) -> TagId;
    fn op_id(&mut self) -> OpId;
}
