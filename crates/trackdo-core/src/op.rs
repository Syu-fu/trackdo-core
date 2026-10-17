//! Operations: the unit of replication between replicas.

use crate::hlc::Hlc;
use crate::task::{TaskChange, TaskId};
use crate::time_entry::{TimeEntryChange, TimeEntryId};

/// Unique per operation, so retransmitted operations can be deduplicated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OpId(pub u128);

/// Server-assigned position in the operation log. Strictly increasing per user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Seq(pub u64);

/// A write of one field of one entity at one point in time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Op {
    pub op_id: OpId,
    pub hlc: Hlc,
    pub target: OpTarget,
}

/// The entity an operation writes to, and the field change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpTarget {
    Task {
        task_id: TaskId,
        change: TaskChange,
    },
    /// Every time entry write carries its task, so an entry can be attributed
    /// even when its first write is the only one received so far.
    TimeEntry {
        entry_id: TimeEntryId,
        task_id: TaskId,
        change: TimeEntryChange,
    },
}

impl Op {
    pub const fn task(op_id: OpId, task_id: TaskId, hlc: Hlc, change: TaskChange) -> Self {
        Self {
            op_id,
            hlc,
            target: OpTarget::Task { task_id, change },
        }
    }

    pub const fn time_entry(
        op_id: OpId,
        entry_id: TimeEntryId,
        task_id: TaskId,
        hlc: Hlc,
        change: TimeEntryChange,
    ) -> Self {
        Self {
            op_id,
            hlc,
            target: OpTarget::TimeEntry {
                entry_id,
                task_id,
                change,
            },
        }
    }
}
