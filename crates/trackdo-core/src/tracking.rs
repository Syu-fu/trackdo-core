//! Time tracking state derived from tasks and time entries.
//!
//! Field-wise merging cannot keep cross-entity invariants, so after syncing
//! there may be several running entries, or a running entry on a deleted
//! task. Such entries are derived to have stopped, deterministically, so every
//! replica shows the same result without coordination.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::task::{Task, TaskId};
use crate::time_entry::{TimeEntry, TimeEntryId, Timestamp};

/// A time entry as it should be presented, with derived stops applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeEntryView<'a> {
    entry: &'a TimeEntry,
    derived_stop: Option<Timestamp>,
}

impl<'a> TimeEntryView<'a> {
    pub(crate) const fn new(entry: &'a TimeEntry, derived_stop: Option<Timestamp>) -> Self {
        Self {
            entry,
            derived_stop,
        }
    }

    /// The stored entry, without derived stops.
    pub const fn entry(&self) -> &'a TimeEntry {
        self.entry
    }

    pub const fn id(&self) -> TimeEntryId {
        self.entry.id()
    }

    pub const fn task_id(&self) -> TaskId {
        self.entry.task_id()
    }

    pub fn started_at(&self) -> Option<Timestamp> {
        self.entry.started_at()
    }

    pub fn stopped_at(&self) -> Option<Timestamp> {
        self.entry.stopped_at().or(self.derived_stop)
    }

    pub fn is_running(&self) -> bool {
        self.entry.is_running() && self.derived_stop.is_none()
    }

    /// The stop time is derived rather than stored. Each replica writes such
    /// stops back once it notices them.
    pub const fn is_stop_derived(&self) -> bool {
        self.derived_stop.is_some()
    }

    pub fn needs_attention(&self) -> bool {
        self.entry.needs_attention()
    }

    /// Measured time, counting up to `now` while running.
    pub fn duration(&self, now: Timestamp) -> Duration {
        // A derived stop only exists for an entry still running in storage,
        // so measuring that entry "up to" the stop gives its duration.
        self.entry.duration(self.derived_stop.unwrap_or(now))
    }
}

/// Stop times for running entries that must be treated as stopped:
///
/// - When several entries run, the one started last wins (ties broken by
///   id) and the others stop when it started.
/// - An entry running on a deleted task stops when the task was deleted, or
///   at its own start if that is later.
///
/// Deleted entries are ignored.
pub(crate) fn derived_stops(
    tasks: &BTreeMap<TaskId, Task>,
    entries: &BTreeMap<TimeEntryId, TimeEntry>,
) -> BTreeMap<TimeEntryId, Timestamp> {
    let running: Vec<_> = entries
        .values()
        .filter(|entry| !entry.is_deleted() && entry.is_running())
        .filter_map(|entry| {
            let started = entry.started_at()?;
            Some((entry.id(), started, deleted_at(tasks, entry.task_id())))
        })
        .collect();

    let orphan_stops = running
        .iter()
        .filter_map(|&(id, started, deleted)| deleted.map(|deleted| (id, started.max(deleted))));
    let live: Vec<_> = running
        .iter()
        .filter(|(_, _, deleted)| deleted.is_none())
        .map(|&(id, started, _)| (started, id))
        .collect();
    let winner = live.iter().max().copied();
    let loser_stops = winner.into_iter().flat_map(|(winner_start, winner_id)| {
        live.iter()
            .filter(move |&&(_, id)| id != winner_id)
            .map(move |&(_, id)| (id, winner_start))
    });
    orphan_stops.chain(loser_stops).collect()
}

/// When the task was deleted, by the wall time of its deletion.
fn deleted_at(tasks: &BTreeMap<TaskId, Task>, task_id: TaskId) -> Option<Timestamp> {
    let task = tasks.get(&task_id).filter(|task| task.is_deleted())?;
    let register = task.deleted_register()?;
    Some(Timestamp(register.hlc().wall_ms()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hlc::{DeviceId, Hlc};
    use crate::task::TaskChange;
    use crate::time_entry::TimeEntryChange;
    use proptest::prelude::*;

    const LIVE: TaskId = TaskId(1);
    const GONE: TaskId = TaskId(2);

    fn at(wall: u64) -> Hlc {
        Hlc::new(wall, 0, DeviceId(1))
    }

    fn ms(millis: u64) -> Timestamp {
        Timestamp(millis)
    }

    fn tasks(deleted_at: u64) -> BTreeMap<TaskId, Task> {
        let gone = Task::empty(GONE).apply(&TaskChange::Deleted(true), at(deleted_at));
        BTreeMap::from([(LIVE, Task::empty(LIVE)), (GONE, gone)])
    }

    fn entry(id: u128, task: TaskId, started: u64, stopped: Option<u64>) -> TimeEntry {
        TimeEntry::empty(TimeEntryId(id), task)
            .apply(&TimeEntryChange::StartedAt(ms(started)), at(1))
            .apply(&TimeEntryChange::StoppedAt(stopped.map(ms)), at(1))
    }

    fn by_id(entries: impl IntoIterator<Item = TimeEntry>) -> BTreeMap<TimeEntryId, TimeEntry> {
        entries.into_iter().map(|e| (e.id(), e)).collect()
    }

    fn stops(pairs: &[(u128, u64)]) -> BTreeMap<TimeEntryId, Timestamp> {
        pairs
            .iter()
            .map(|(id, stop)| (TimeEntryId(*id), ms(*stop)))
            .collect()
    }

    #[test]
    fn single_running_entry_keeps_running() {
        let entries = by_id([entry(1, LIVE, 100, None), entry(2, LIVE, 50, Some(80))]);

        assert_eq!(derived_stops(&tasks(0), &entries), stops(&[]));
    }

    #[test]
    fn latest_start_wins_and_others_stop_when_it_started() {
        let entries = by_id([
            entry(1, LIVE, 100, None),
            entry(2, LIVE, 300, None),
            entry(3, LIVE, 200, None),
        ]);

        assert_eq!(
            derived_stops(&tasks(0), &entries),
            stops(&[(1, 300), (3, 300)])
        );
    }

    #[test]
    fn equal_starts_are_broken_by_id() {
        let entries = by_id([entry(1, LIVE, 100, None), entry(2, LIVE, 100, None)]);

        assert_eq!(derived_stops(&tasks(0), &entries), stops(&[(1, 100)]));
    }

    #[test]
    fn deleted_entries_take_no_part() {
        let deleted = entry(2, LIVE, 300, None).apply(&TimeEntryChange::Deleted(true), at(2));
        let entries = by_id([entry(1, LIVE, 100, None), deleted]);

        assert_eq!(derived_stops(&tasks(0), &entries), stops(&[]));
    }

    #[test]
    fn running_entry_on_deleted_task_stops_at_deletion() {
        let entries = by_id([entry(1, GONE, 100, None)]);

        assert_eq!(derived_stops(&tasks(250), &entries), stops(&[(1, 250)]));
    }

    #[test]
    fn deletion_before_start_stops_entry_at_its_start() {
        let entries = by_id([entry(1, GONE, 100, None)]);

        assert_eq!(derived_stops(&tasks(40), &entries), stops(&[(1, 100)]));
    }

    #[test]
    fn entry_on_deleted_task_does_not_stop_others() {
        let entries = by_id([entry(1, LIVE, 100, None), entry(2, GONE, 200, None)]);

        assert_eq!(derived_stops(&tasks(250), &entries), stops(&[(2, 250)]));
    }

    #[test]
    fn entry_of_unknown_task_counts_as_live() {
        let entries = by_id([entry(1, TaskId(99), 100, None), entry(2, LIVE, 200, None)]);

        assert_eq!(derived_stops(&tasks(0), &entries), stops(&[(1, 200)]));
    }

    #[test]
    fn view_applies_derived_stop() {
        let stored = entry(1, LIVE, 100, None);

        let view = TimeEntryView::new(&stored, Some(ms(300)));

        assert_eq!(view.entry(), &stored);
        assert_eq!(view.id(), TimeEntryId(1));
        assert_eq!(view.task_id(), LIVE);
        assert_eq!(view.started_at(), Some(ms(100)));
        assert_eq!(view.stopped_at(), Some(ms(300)));
        assert!(!view.is_running());
        assert!(view.is_stop_derived());
        assert_eq!(view.duration(ms(9_000)), Duration::from_millis(200));
    }

    #[test]
    fn view_without_derived_stop_matches_entry() {
        let stored = entry(1, LIVE, 100, None);

        let view = TimeEntryView::new(&stored, None);

        assert_eq!(view.stopped_at(), None);
        assert!(view.is_running());
        assert!(!view.is_stop_derived());
        assert_eq!(view.duration(ms(400)), Duration::from_millis(300));
    }

    #[test]
    fn view_reports_inverted_interval() {
        let stored = entry(1, LIVE, 300, Some(100));

        assert!(TimeEntryView::new(&stored, None).needs_attention());
    }

    fn any_entry() -> impl Strategy<Value = TimeEntry> {
        (
            0u128..6,
            prop::sample::select(vec![LIVE, GONE, TaskId(3)]),
            0u64..5,
            prop::option::of(0u64..5),
            any::<bool>(),
        )
            .prop_map(|(id, task, started, stopped, deleted)| {
                entry(id, task, started, stopped).apply(&TimeEntryChange::Deleted(deleted), at(2))
            })
    }

    proptest! {
        #[test]
        fn at_most_one_live_entry_runs(
            entries in prop::collection::vec(any_entry(), 0..8),
            deleted_at in 0u64..5,
        ) {
            let tasks = tasks(deleted_at);
            let entries = by_id(entries);
            let derived = derived_stops(&tasks, &entries);

            let running: Vec<_> = entries
                .values()
                .filter(|e| !e.is_deleted())
                .map(|e| TimeEntryView::new(e, derived.get(&e.id()).copied()))
                .filter(TimeEntryView::is_running)
                .collect();

            prop_assert!(running.len() <= 1);
            prop_assert!(running.iter().all(|view| view.task_id() != GONE));
        }

        #[test]
        fn derived_stops_never_invert_or_touch_stopped_entries(
            entries in prop::collection::vec(any_entry(), 0..8),
            deleted_at in 0u64..5,
        ) {
            let entries = by_id(entries);

            for (id, stop) in derived_stops(&tasks(deleted_at), &entries) {
                let entry = &entries[&id];
                prop_assert!(entry.is_running() && !entry.is_deleted());
                prop_assert!(entry.started_at().is_some_and(|started| started <= stop));
            }
        }
    }
}
