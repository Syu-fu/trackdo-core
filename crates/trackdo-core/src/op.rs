//! Operations: the unit of replication between replicas.

use crate::hlc::Hlc;
use crate::task::{FieldChange, TaskId};

/// Unique per operation, so retransmitted operations can be deduplicated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OpId(pub u128);

/// A write of one task field at one point in time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Op {
    pub op_id: OpId,
    pub task_id: TaskId,
    pub hlc: Hlc,
    pub change: FieldChange,
}
