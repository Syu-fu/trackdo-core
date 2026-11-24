//! Read access to time tracking, with derived stops applied.

use std::time::Duration;

use super::Replica;
use crate::env::{Clock, IdGen};
use crate::task::{Task, TaskId};
use crate::time_entry::{TimeEntry, Timestamp};
use crate::tracking::{TimeEntryView, derived_stops};

impl<C: Clock, G: IdGen> Replica<C, G> {
    /// The single running timer, if any.
    pub fn running_entry(&self) -> Option<TimeEntryView<'_>> {
        self.views(|_| true)
            .into_iter()
            .find(TimeEntryView::is_running)
    }

    /// Whether the running timer belongs to `task_id`.
    pub fn is_in_progress(&self, task_id: TaskId) -> bool {
        self.running_entry()
            .is_some_and(|running| running.task_id() == task_id)
    }

    /// Entries of `task_id` that are not deleted, ordered by start time.
    pub fn entries_for(&self, task_id: TaskId) -> Vec<TimeEntryView<'_>> {
        self.views(|entry| entry.task_id() == task_id)
    }

    /// Entries that count towards reports: not deleted and not belonging to
    /// a task that counts as deleted. Ordered by start time.
    pub fn tracked_entries(&self) -> Vec<TimeEntryView<'_>> {
        let tree = self.hierarchy();
        self.views(|entry| !tree.is_deleted(entry.task_id()))
    }

    /// Total time spent on `task_id`, counting a running timer up to `now`.
    /// Zero for a task that counts as deleted.
    pub fn task_total(&self, task_id: TaskId, now: Timestamp) -> Duration {
        if self.hierarchy().is_deleted(task_id) {
            return Duration::ZERO;
        }
        self.entries_for(task_id)
            .iter()
            .map(|view| view.duration(now))
            .sum()
    }

    /// Time spent on `task_id` since its current occurrence began, counting a
    /// running timer up to `now`. For a task never completed while repeating,
    /// this is its total. Zero for a task that counts as deleted.
    pub fn occurrence_total(&self, task_id: TaskId, now: Timestamp) -> Duration {
        if self.hierarchy().is_deleted(task_id) {
            return Duration::ZERO;
        }
        let since = self
            .tasks
            .get(&task_id)
            .and_then(Task::occurrence_start)
            .unwrap_or(Timestamp(0));
        self.entries_for(task_id)
            .iter()
            .map(|view| view.duration_since(since, now))
            .sum()
    }

    /// Views of non-deleted entries that `keep`, ordered by start time.
    fn views(&self, keep: impl Fn(&TimeEntry) -> bool) -> Vec<TimeEntryView<'_>> {
        let derived = derived_stops(&self.tasks, &self.entries);
        let mut views: Vec<_> = self
            .entries
            .values()
            .filter(|entry| !entry.is_deleted() && keep(entry))
            .map(|entry| TimeEntryView::new(entry, derived.get(&entry.id()).copied()))
            .collect();
        views.sort_by_key(|view| (view.started_at(), view.id()));
        views
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use crate::hlc::Hlc;
    use crate::op::{Op, OpId, Seq};
    use crate::recurrence::RecurrenceBasis;
    use crate::time_entry::{TimeEntryChange, TimeEntryId};

    fn ms(millis: u64) -> Timestamp {
        Timestamp(millis)
    }

    fn ids(views: &[TimeEntryView<'_>]) -> Vec<TimeEntryId> {
        views.iter().map(TimeEntryView::id).collect()
    }

    /// Another device started a timer on `task` at `started`.
    fn remote_start(replica: &mut TestReplica, task: TaskId, started: u64) -> TimeEntryId {
        let entry = TimeEntryId(9_000 + u128::from(started));
        let seq = replica.last_seq().map_or(1, |Seq(seq)| seq + 1);
        for (offset, change) in [
            TimeEntryChange::StartedAt(ms(started)),
            TimeEntryChange::StoppedAt(None),
        ]
        .into_iter()
        .enumerate()
        {
            let op = Op::time_entry(
                OpId(entry.0 * 10 + offset as u128),
                entry,
                task,
                Hlc::new(started, offset as u32, OTHER),
                change,
            );
            replica.apply_remote(&op, Seq(seq + offset as u64)).unwrap();
        }
        entry
    }

    #[test]
    fn nothing_runs_initially() {
        let replica = replica();

        assert_eq!(replica.running_entry(), None);
    }

    #[test]
    fn started_timer_is_running_and_in_progress() {
        let mut replica = replica();
        let task = replica.create("work").unwrap();
        let other = replica.create("other").unwrap();

        let id = replica.start(task).unwrap();

        assert_eq!(replica.running_entry().map(|view| view.id()), Some(id));
        assert!(replica.is_in_progress(task));
        assert!(!replica.is_in_progress(other));
    }

    #[test]
    fn later_remote_start_wins_over_local_timer() {
        let mut replica = replica();
        let local_task = replica.create("local").unwrap();
        let remote_task = replica.create("remote").unwrap();
        let local = replica.start(local_task).unwrap();

        let remote = remote_start(&mut replica, remote_task, NOW + 10);

        assert_eq!(replica.running_entry().map(|view| view.id()), Some(remote));
        assert!(!replica.is_in_progress(local_task));
        let view = replica.entries_for(local_task)[0];
        assert_eq!(view.id(), local);
        assert_eq!(view.stopped_at(), Some(ms(NOW + 10)));
    }

    #[test]
    fn stopped_timer_is_not_in_progress() {
        let mut replica = replica();
        let task = replica.create("work").unwrap();
        let id = replica.start(task).unwrap();

        replica.stop(id).unwrap();

        assert_eq!(replica.running_entry(), None);
        assert!(!replica.is_in_progress(task));
    }

    #[test]
    fn entries_for_lists_live_entries_by_start_time() {
        let mut replica = replica();
        let task = replica.create("work").unwrap();
        let other = replica.create("other").unwrap();
        let late = replica.add_entry(task, ms(500), ms(600)).unwrap();
        let early = replica.add_entry(task, ms(100), ms(200)).unwrap();
        let deleted = replica.add_entry(task, ms(300), ms(400)).unwrap();
        replica.add_entry(other, ms(100), ms(200)).unwrap();
        replica.delete_entry(deleted).unwrap();

        assert_eq!(ids(&replica.entries_for(task)), vec![early, late]);
    }

    #[test]
    fn task_total_sums_entries_and_running_time() {
        let mut replica = replica();
        let task = replica.create("work").unwrap();
        replica.add_entry(task, ms(100), ms(400)).unwrap();
        replica.start(task).unwrap();

        assert_eq!(
            replica.task_total(task, ms(NOW + 50)),
            Duration::from_millis(300 + 50)
        );
    }

    #[test]
    fn task_total_respects_derived_stop() {
        let mut replica = replica();
        let task = replica.create("local").unwrap();
        let remote_task = replica.create("remote").unwrap();
        replica.start(task).unwrap();
        remote_start(&mut replica, remote_task, NOW + 10);

        assert_eq!(
            replica.task_total(task, ms(NOW + 5_000)),
            Duration::from_millis(10)
        );
    }

    #[test]
    fn deleted_task_is_excluded_from_totals() {
        let mut replica = replica();
        let task = replica.create("work").unwrap();
        let kept = replica.create("kept").unwrap();
        replica.add_entry(task, ms(100), ms(400)).unwrap();
        let counted = replica.add_entry(kept, ms(100), ms(200)).unwrap();

        replica.delete(task).unwrap();

        assert_eq!(replica.task_total(task, ms(NOW)), Duration::ZERO);
        assert_eq!(replica.entries_for(task).len(), 1);
        assert_eq!(ids(&replica.tracked_entries()), vec![counted]);
    }

    /// A task repeating daily with 300ms tracked before a timer ran from
    /// `NOW` until it was completed at 1_600, then 200ms more, of which 100ms
    /// after the completion, and a timer running since 2_000.
    fn repeated_task() -> (TestReplica, TaskId) {
        let mut replica = replica();
        let task = replica.create("stretch").unwrap();
        replica
            .set_recurrence(task, "FREQ=DAILY", RecurrenceBasis::Due)
            .unwrap();
        replica.add_entry(task, ms(100), ms(400)).unwrap();
        replica.start(task).unwrap();
        set_now(&mut replica, 1_600);
        replica.complete(task).unwrap();
        set_now(&mut replica, 2_000);
        replica.add_entry(task, ms(1_500), ms(1_700)).unwrap();
        replica.start(task).unwrap();
        (replica, task)
    }

    #[test]
    fn occurrence_total_counts_time_since_the_last_completion() {
        let (replica, task) = repeated_task();

        assert_eq!(
            replica.occurrence_total(task, ms(2_050)),
            Duration::from_millis(100 + 50)
        );
        assert_eq!(
            replica.task_total(task, ms(2_050)),
            Duration::from_millis(300 + 600 + 200 + 50)
        );
    }

    #[test]
    fn occurrence_total_of_a_task_never_repeated_is_its_total() {
        let mut replica = replica();
        let task = replica.create("work").unwrap();
        replica.add_entry(task, ms(100), ms(400)).unwrap();
        replica.complete(task).unwrap();

        assert_eq!(
            replica.occurrence_total(task, ms(NOW)),
            Duration::from_millis(300)
        );
    }

    #[test]
    fn occurrence_total_of_a_deleted_task_is_zero() {
        let (mut replica, task) = repeated_task();

        replica.delete(task).unwrap();

        assert_eq!(replica.occurrence_total(task, ms(2_050)), Duration::ZERO);
    }

    #[test]
    fn tracked_entries_are_ordered_by_start_time() {
        let mut replica = replica();
        let task = replica.create("work").unwrap();
        let other = replica.create("other").unwrap();
        let late = replica.add_entry(task, ms(500), ms(600)).unwrap();
        let early = replica.add_entry(other, ms(100), ms(200)).unwrap();

        assert_eq!(ids(&replica.tracked_entries()), vec![early, late]);
    }
}
