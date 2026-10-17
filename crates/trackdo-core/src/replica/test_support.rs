//! Deterministic environment shared by replica tests.

use super::Replica;
use crate::env::{Clock, IdGen};
use crate::hlc::DeviceId;
use crate::op::{OpId, OpTarget};
use crate::task::{TaskChange, TaskId};
use crate::time_entry::TimeEntryId;
use crate::title::Title;

pub(super) const OTHER: DeviceId = DeviceId(8);
pub(super) const DEVICE: DeviceId = DeviceId(7);
pub(super) const NOW: u64 = 1_000;

pub(super) struct FixedClock(pub(super) u64);

impl Clock for FixedClock {
    fn now_millis(&self) -> u64 {
        self.0
    }
}

#[derive(Default)]
pub(super) struct SequentialIds(pub(super) u128);

impl IdGen for SequentialIds {
    fn task_id(&mut self) -> TaskId {
        self.0 += 1;
        TaskId(self.0)
    }

    fn time_entry_id(&mut self) -> TimeEntryId {
        self.0 += 1;
        TimeEntryId(self.0)
    }

    fn op_id(&mut self) -> OpId {
        self.0 += 1;
        OpId(self.0)
    }
}

pub(super) type TestReplica = Replica<FixedClock, SequentialIds>;

pub(super) fn replica() -> TestReplica {
    Replica::new(DEVICE, FixedClock(NOW), SequentialIds::default())
}

pub(super) fn title(s: &str) -> Title {
    Title::new(s).unwrap()
}

/// Moves the clock of `replica`, e.g. to simulate elapsed time or a reset.
pub(super) fn set_now(replica: &mut TestReplica, now: u64) {
    replica.clock.0 = now;
}

pub(super) fn changes(replica: &TestReplica) -> Vec<TaskChange> {
    replica
        .pending_ops()
        .iter()
        .filter_map(|op| match &op.target {
            OpTarget::Task { change, .. } => Some(change.clone()),
            OpTarget::TimeEntry { .. } => None,
        })
        .collect()
}
