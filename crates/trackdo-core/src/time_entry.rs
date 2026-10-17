//! Time entry: a measured interval of work on a task.

use std::time::Duration;

use crate::hlc::Hlc;
use crate::lww::{Lww, write};
use crate::task::TaskId;

/// Wall-clock instant as milliseconds since the Unix epoch, in UTC.
///
/// Unlike [`Hlc`], this is user-visible data: it is what the user sees and
/// edits, and it is never used to order writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimeEntryId(pub u128);

/// A write to a single time entry field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeEntryChange {
    StartedAt(Timestamp),
    /// `None` means the timer is running.
    StoppedAt(Option<Timestamp>),
    Deleted(bool),
}

/// Start and stop are separate registers so that stopping a timer and
/// concurrently correcting its start time are both kept.
///
/// `task_id` is fixed when the entry is created and never changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeEntry {
    id: TimeEntryId,
    task_id: TaskId,
    started_at: Option<Lww<Timestamp>>,
    stopped_at: Option<Lww<Option<Timestamp>>>,
    deleted: Option<Lww<bool>>,
}

impl TimeEntry {
    /// An entry that has not received any write yet.
    pub const fn empty(id: TimeEntryId, task_id: TaskId) -> Self {
        Self {
            id,
            task_id,
            started_at: None,
            stopped_at: None,
            deleted: None,
        }
    }

    pub const fn id(&self) -> TimeEntryId {
        self.id
    }

    pub const fn task_id(&self) -> TaskId {
        self.task_id
    }

    /// `None` until a start write has been received.
    pub fn started_at(&self) -> Option<Timestamp> {
        self.started_at.as_ref().map(|started| *started.value())
    }

    /// `None` while running or before any stop write has been received.
    pub fn stopped_at(&self) -> Option<Timestamp> {
        self.stopped_at
            .as_ref()
            .and_then(|stopped| *stopped.value())
    }

    pub fn is_running(&self) -> bool {
        self.started_at.is_some() && self.stopped_at().is_none()
    }

    pub fn is_deleted(&self) -> bool {
        self.deleted
            .as_ref()
            .is_some_and(|deleted| *deleted.value())
    }

    /// The stop time precedes the start time. This can only result from
    /// concurrent edits on different devices and is left for the user to fix.
    pub fn needs_attention(&self) -> bool {
        matches!(
            (self.started_at(), self.stopped_at()),
            (Some(started), Some(stopped)) if stopped < started
        )
    }

    /// Measured time, counting up to `now` while running. Never negative.
    pub fn duration(&self, now: Timestamp) -> Duration {
        let Some(Timestamp(started)) = self.started_at() else {
            return Duration::ZERO;
        };
        let Timestamp(end) = self.stopped_at().unwrap_or(now);
        Duration::from_millis(end.saturating_sub(started))
    }

    /// Entry after applying `change` written at `hlc`. Stale writes are ignored.
    pub fn apply(&self, change: &TimeEntryChange, hlc: Hlc) -> Self {
        match change {
            TimeEntryChange::StartedAt(started) => Self {
                started_at: Some(write(&self.started_at, *started, hlc)),
                ..self.clone()
            },
            TimeEntryChange::StoppedAt(stopped) => Self {
                stopped_at: Some(write(&self.stopped_at, *stopped, hlc)),
                ..self.clone()
            },
            TimeEntryChange::Deleted(deleted) => Self {
                deleted: Some(write(&self.deleted, *deleted, hlc)),
                ..self.clone()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hlc::DeviceId;
    use proptest::prelude::*;

    const ID: TimeEntryId = TimeEntryId(7);
    const TASK: TaskId = TaskId(42);
    const A: DeviceId = DeviceId(1);
    const B: DeviceId = DeviceId(2);

    fn at(wall: u64, device: DeviceId) -> Hlc {
        Hlc::new(wall, 0, device)
    }

    fn ms(millis: u64) -> Timestamp {
        Timestamp(millis)
    }

    fn running_since(started: u64) -> TimeEntry {
        TimeEntry::empty(ID, TASK)
            .apply(&TimeEntryChange::StartedAt(ms(started)), at(1, A))
            .apply(&TimeEntryChange::StoppedAt(None), at(1, A))
    }

    #[test]
    fn empty_entry_has_no_interval() {
        let entry = TimeEntry::empty(ID, TASK);

        assert_eq!(entry.id(), ID);
        assert_eq!(entry.task_id(), TASK);
        assert_eq!(entry.started_at(), None);
        assert_eq!(entry.stopped_at(), None);
        assert!(!entry.is_running());
        assert!(!entry.is_deleted());
        assert!(!entry.needs_attention());
        assert_eq!(entry.duration(ms(1_000)), Duration::ZERO);
    }

    #[test]
    fn started_entry_without_stop_is_running() {
        let entry = running_since(1_000);

        assert!(entry.is_running());
        assert_eq!(entry.started_at(), Some(ms(1_000)));
        assert_eq!(entry.stopped_at(), None);
    }

    #[test]
    fn start_alone_counts_as_running() {
        let entry =
            TimeEntry::empty(ID, TASK).apply(&TimeEntryChange::StartedAt(ms(1_000)), at(1, A));

        assert!(entry.is_running());
    }

    #[test]
    fn stop_without_start_is_not_running() {
        let entry = TimeEntry::empty(ID, TASK).apply(&TimeEntryChange::StoppedAt(None), at(1, A));

        assert!(!entry.is_running());
    }

    #[test]
    fn running_duration_grows_with_now() {
        let entry = running_since(1_000);

        assert_eq!(entry.duration(ms(4_500)), Duration::from_millis(3_500));
    }

    #[test]
    fn running_duration_is_zero_when_now_precedes_start() {
        let entry = running_since(1_000);

        assert_eq!(entry.duration(ms(500)), Duration::ZERO);
    }

    #[test]
    fn stopped_duration_ignores_now() {
        let entry =
            running_since(1_000).apply(&TimeEntryChange::StoppedAt(Some(ms(3_000))), at(2, A));

        assert!(!entry.is_running());
        assert_eq!(entry.stopped_at(), Some(ms(3_000)));
        assert_eq!(entry.duration(ms(99_000)), Duration::from_millis(2_000));
        assert!(!entry.needs_attention());
    }

    #[test]
    fn zero_length_entry_is_valid() {
        let entry =
            running_since(1_000).apply(&TimeEntryChange::StoppedAt(Some(ms(1_000))), at(2, A));

        assert_eq!(entry.duration(ms(5_000)), Duration::ZERO);
        assert!(!entry.needs_attention());
    }

    #[test]
    fn concurrent_start_and_stop_edits_are_both_kept() {
        let base =
            running_since(1_000).apply(&TimeEntryChange::StoppedAt(Some(ms(3_000))), at(2, A));

        let entry = base
            .apply(&TimeEntryChange::StartedAt(ms(4_000)), at(10, A))
            .apply(&TimeEntryChange::StoppedAt(Some(ms(2_000))), at(11, B));

        assert_eq!(entry.started_at(), Some(ms(4_000)));
        assert_eq!(entry.stopped_at(), Some(ms(2_000)));
    }

    #[test]
    fn stop_before_start_counts_as_zero_and_needs_attention() {
        let entry =
            running_since(4_000).apply(&TimeEntryChange::StoppedAt(Some(ms(2_000))), at(2, B));

        assert_eq!(entry.duration(ms(9_000)), Duration::ZERO);
        assert!(entry.needs_attention());
    }

    #[test]
    fn stale_write_is_ignored() {
        let entry = running_since(1_000)
            .apply(&TimeEntryChange::StartedAt(ms(2_000)), at(20, A))
            .apply(&TimeEntryChange::StartedAt(ms(3_000)), at(10, B));

        assert_eq!(entry.started_at(), Some(ms(2_000)));
    }

    #[test]
    fn stopping_is_undone_only_by_a_newer_write() {
        let entry = running_since(1_000)
            .apply(&TimeEntryChange::StoppedAt(Some(ms(2_000))), at(5, A))
            .apply(&TimeEntryChange::StoppedAt(None), at(3, B));

        assert!(!entry.is_running());
    }

    #[test]
    fn deletion_follows_last_write() {
        let deleted = running_since(1_000).apply(&TimeEntryChange::Deleted(true), at(2, A));
        let restored = deleted.apply(&TimeEntryChange::Deleted(false), at(3, B));

        assert!(deleted.is_deleted());
        assert!(!restored.is_deleted());
    }

    fn change() -> impl Strategy<Value = TimeEntryChange> {
        let time = (0u64..4).prop_map(Timestamp);
        prop_oneof![
            time.clone().prop_map(TimeEntryChange::StartedAt),
            prop::option::of(time).prop_map(TimeEntryChange::StoppedAt),
            any::<bool>().prop_map(TimeEntryChange::Deleted),
        ]
    }

    fn write() -> impl Strategy<Value = (TimeEntryChange, Hlc)> {
        (change(), 0u64..4, 0u32..2, 0u128..3).prop_map(|(change, wall, counter, device)| {
            (change, Hlc::new(wall, counter, DeviceId(device)))
        })
    }

    fn apply_all(writes: &[(TimeEntryChange, Hlc)]) -> TimeEntry {
        writes
            .iter()
            .fold(TimeEntry::empty(ID, TASK), |entry, (change, hlc)| {
                entry.apply(change, *hlc)
            })
    }

    proptest! {
        #[test]
        fn apply_order_does_not_matter(
            (writes, shuffled) in prop::collection::vec(write(), 0..12)
                .prop_flat_map(|w| (Just(w.clone()), Just(w).prop_shuffle()))
        ) {
            prop_assert_eq!(apply_all(&writes), apply_all(&shuffled));
        }

        #[test]
        fn reapplying_writes_is_idempotent(writes in prop::collection::vec(write(), 0..12)) {
            let doubled: Vec<_> = writes.iter().chain(writes.iter()).cloned().collect();

            prop_assert_eq!(apply_all(&writes), apply_all(&doubled));
        }

        #[test]
        fn stopped_duration_is_independent_of_now(
            writes in prop::collection::vec(write(), 0..12),
            now in 0u64..8,
            later in 0u64..8,
        ) {
            let entry = apply_all(&writes);
            prop_assume!(!entry.is_running());

            prop_assert_eq!(entry.duration(Timestamp(now)), entry.duration(Timestamp(later)));
        }
    }
}
