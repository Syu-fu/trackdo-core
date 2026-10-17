//! Environment capabilities injected by the host, keeping the core free of I/O.

use crate::op::OpId;
use crate::task::TaskId;

/// Source of physical time.
pub trait Clock {
    /// Milliseconds since the Unix epoch. May go backwards; the HLC absorbs it.
    fn now_millis(&self) -> u64;
}

/// Source of globally unique identifiers, typically UUIDv7.
pub trait IdGen {
    fn task_id(&mut self) -> TaskId;
    fn op_id(&mut self) -> OpId;
}
