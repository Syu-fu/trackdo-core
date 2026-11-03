//! Commands making tasks repeat.

use super::{CommandError, Replica};
use crate::env::{Clock, IdGen};
use crate::op::OpTarget;
use crate::recurrence::{Recurrence, RecurrenceBasis};
use crate::rrule::RRule;
use crate::task::{Task, TaskChange, TaskId, TaskStatus};

impl<C: Clock, G: IdGen> Replica<C, G> {
    /// Makes `id` repeat by `rule`, an RFC 5545 recurrence rule of whole
    /// days, starting from its due date. A task without one becomes due
    /// today, which is then the first occurrence.
    pub fn set_recurrence(
        &mut self,
        id: TaskId,
        rule: &str,
        basis: RecurrenceBasis,
    ) -> Result<(), CommandError> {
        self.ensure_editable(id)?;
        let rule = RRule::parse(rule)?;
        let due = self.tasks.get(&id).and_then(Task::due);
        let start = due.unwrap_or_else(|| self.clock.today());
        let recurrence = Recurrence::new(rule, start, basis)?;
        let mut changes = vec![TaskChange::Recurrence(Some(recurrence))];
        if due.is_none() {
            changes.push(TaskChange::Due(Some(start)));
        }
        self.emit_task(id, changes)
    }

    /// The writes completing `id` makes, apart from stopping its timer.
    ///
    /// A repeating task becomes due on its next occurrence and stays to do,
    /// unscheduled, and the completed tasks below it are reopened, so a
    /// checklist under it starts over. Once the rule has ended, the task is
    /// completed like any other.
    pub(super) fn completion(&self, id: TaskId) -> Vec<OpTarget> {
        let task = &self.tasks[&id];
        let next = task
            .recurrence()
            .and_then(|recurrence| recurrence.next_due(task.due(), self.clock.today()));
        let Some(next) = next else {
            return vec![OpTarget::Task {
                task_id: id,
                change: TaskChange::Status(TaskStatus::Done),
            }];
        };
        let mut changes = vec![TaskChange::Due(Some(next))];
        if task.scheduled_at().is_some() {
            changes.push(TaskChange::ScheduledAt(None));
        }
        if task.status() == TaskStatus::Done {
            changes.push(TaskChange::Status(TaskStatus::Todo));
        }
        let own = changes.into_iter().map(|change| OpTarget::Task {
            task_id: id,
            change,
        });
        let reopened = self
            .completed_descendants(id)
            .into_iter()
            .map(|task_id| OpTarget::Task {
                task_id,
                change: TaskChange::Status(TaskStatus::Todo),
            });
        own.chain(reopened).collect()
    }

    /// Tasks below `id` that are done and not deleted, depth first.
    fn completed_descendants(&self, id: TaskId) -> Vec<TaskId> {
        let tree = self.hierarchy();
        let mut found = Vec::new();
        let mut stack: Vec<TaskId> = tree.children(id).iter().rev().copied().collect();
        while let Some(task) = stack.pop() {
            if tree.is_deleted(task) {
                continue;
            }
            if self.tasks[&task].status() == TaskStatus::Done {
                found.push(task);
            }
            stack.extend(tree.children(task).iter().rev());
        }
        found
    }

    /// Keeps the due date as it is.
    pub fn clear_recurrence(&mut self, id: TaskId) -> Result<(), CommandError> {
        self.ensure_editable(id)?;
        self.emit_task(id, vec![TaskChange::Recurrence(None)])
    }
}

#[cfg(test)]
mod tests {
    use super::super::CommandError;
    use super::super::test_support::*;
    use crate::date::Date;
    use crate::op::OpTarget;
    use crate::recurrence::{Recurrence, RecurrenceBasis, RecurrenceError};
    use crate::rrule::{RRule, RRuleError};
    use crate::task::{TaskChange, TaskId, TaskStatus};
    use crate::time_entry::Timestamp;

    const MILLIS_PER_DAY: u64 = 86_400_000;

    fn day(month: u8, day: u8) -> Date {
        Date::new(2026, month, day).unwrap()
    }

    fn recurrence(rule: &str, start: Date, basis: RecurrenceBasis) -> Recurrence {
        Recurrence::new(RRule::parse(rule).unwrap(), start, basis).unwrap()
    }

    #[test]
    fn set_recurrence_starts_at_the_due_date() {
        let mut replica = replica();
        let id = replica.create("bins").unwrap();
        replica.set_due(id, Some(day(11, 2))).unwrap();

        replica
            .set_recurrence(id, "freq=weekly", RecurrenceBasis::Due)
            .unwrap();

        let expected = recurrence("FREQ=WEEKLY", day(11, 2), RecurrenceBasis::Due);
        assert_eq!(replica.task(id).unwrap().recurrence(), Some(&expected));
        assert_eq!(
            changes(&replica).last(),
            Some(&TaskChange::Recurrence(Some(expected)))
        );
    }

    #[test]
    fn set_recurrence_without_a_due_date_starts_today() {
        let mut replica = replica();
        set_now(&mut replica, 20_760 * MILLIS_PER_DAY + 1);
        let id = replica.create("haircut").unwrap();
        let before = changes(&replica).len();

        replica
            .set_recurrence(id, "FREQ=WEEKLY;INTERVAL=2", RecurrenceBasis::Completion)
            .unwrap();

        let expected = recurrence(
            "FREQ=WEEKLY;INTERVAL=2",
            day(11, 3),
            RecurrenceBasis::Completion,
        );
        assert_eq!(replica.task(id).unwrap().due(), Some(day(11, 3)));
        assert_eq!(
            changes(&replica)[before..],
            [
                TaskChange::Recurrence(Some(expected)),
                TaskChange::Due(Some(day(11, 3))),
            ]
        );
    }

    #[test]
    fn set_recurrence_rejects_invalid_rules() {
        let mut replica = replica();
        let id = replica.create("a").unwrap();
        let before = replica.pending_ops().len();

        assert_eq!(
            replica.set_recurrence(id, "FREQ=HOURLY", RecurrenceBasis::Due),
            Err(CommandError::RRule(RRuleError::UnsupportedFrequency(
                "HOURLY".to_owned()
            )))
        );
        assert_eq!(
            replica.set_recurrence(id, "FREQ=DAILY;COUNT=3", RecurrenceBasis::Completion),
            Err(CommandError::Recurrence(
                RecurrenceError::CountFromCompletion
            ))
        );
        assert_eq!(replica.pending_ops().len(), before);
    }

    #[test]
    fn clear_recurrence_stops_the_task_repeating() {
        let mut replica = replica();
        let id = replica.create("a").unwrap();
        replica
            .set_recurrence(id, "FREQ=DAILY", RecurrenceBasis::Due)
            .unwrap();

        replica.clear_recurrence(id).unwrap();

        assert_eq!(replica.task(id).unwrap().recurrence(), None);
        assert_eq!(
            changes(&replica).last(),
            Some(&TaskChange::Recurrence(None))
        );
    }

    /// A replica on Tuesday, November 3rd, with a task due Monday the 2nd
    /// repeating every Monday.
    fn weekly_task(basis: RecurrenceBasis) -> (TestReplica, TaskId) {
        let mut replica = replica();
        set_now(&mut replica, 20_760 * MILLIS_PER_DAY + 1);
        let id = replica.create("bins").unwrap();
        replica.set_due(id, Some(day(11, 2))).unwrap();
        replica
            .set_recurrence(id, "FREQ=WEEKLY;BYDAY=MO", basis)
            .unwrap();
        (replica, id)
    }

    fn task_changes(replica: &TestReplica, from: usize) -> Vec<(TaskId, TaskChange)> {
        replica.pending_ops()[from..]
            .iter()
            .filter_map(|op| match &op.target {
                OpTarget::Task { task_id, change } => Some((*task_id, change.clone())),
                OpTarget::TimeEntry { .. } | OpTarget::Tag { .. } => None,
            })
            .collect()
    }

    #[test]
    fn completing_a_repeating_task_moves_it_to_the_next_occurrence() {
        let (mut replica, id) = weekly_task(RecurrenceBasis::Due);
        let before = replica.pending_ops().len();

        replica.complete(id).unwrap();

        let task = replica.task(id).unwrap();
        assert_eq!(task.due(), Some(day(11, 9)));
        assert_eq!(task.status(), TaskStatus::Todo);
        assert_eq!(
            task_changes(&replica, before),
            [(id, TaskChange::Due(Some(day(11, 9))))]
        );
    }

    #[test]
    fn completing_late_skips_the_occurrences_missed() {
        let (mut replica, id) = weekly_task(RecurrenceBasis::Due);
        set_now(&mut replica, (20_760 + 15) * MILLIS_PER_DAY);

        replica.complete(id).unwrap();

        assert_eq!(replica.task(id).unwrap().due(), Some(day(11, 23)));
    }

    #[test]
    fn completion_basis_counts_from_the_day_of_completion() {
        let mut replica = replica();
        set_now(&mut replica, 20_760 * MILLIS_PER_DAY);
        let id = replica.create("haircut").unwrap();
        replica
            .set_recurrence(id, "FREQ=DAILY;INTERVAL=14", RecurrenceBasis::Completion)
            .unwrap();
        set_now(&mut replica, (20_760 + 3) * MILLIS_PER_DAY);

        replica.complete(id).unwrap();

        assert_eq!(replica.task(id).unwrap().due(), Some(day(11, 20)));
    }

    #[test]
    fn completing_a_repeating_task_unschedules_it() {
        let (mut replica, id) = weekly_task(RecurrenceBasis::Due);
        replica.schedule(id, Some(Timestamp(NOW))).unwrap();

        replica.complete(id).unwrap();

        assert_eq!(replica.task(id).unwrap().scheduled_at(), None);
    }

    #[test]
    fn completing_the_last_occurrence_completes_the_task() {
        let mut replica = replica();
        set_now(&mut replica, 20_760 * MILLIS_PER_DAY);
        let id = replica.create("twice").unwrap();
        replica
            .set_recurrence(id, "FREQ=DAILY;COUNT=2", RecurrenceBasis::Due)
            .unwrap();

        replica.complete(id).unwrap();
        assert_eq!(replica.task(id).unwrap().due(), Some(day(11, 4)));
        assert_eq!(replica.task(id).unwrap().status(), TaskStatus::Todo);

        replica.complete(id).unwrap();
        assert_eq!(replica.task(id).unwrap().due(), Some(day(11, 4)));
        assert_eq!(replica.task(id).unwrap().status(), TaskStatus::Done);
    }

    #[test]
    fn completing_a_repeating_task_reopens_its_completed_descendants() {
        let (mut replica, id) = weekly_task(RecurrenceBasis::Due);
        let done_child = replica.create("sort").unwrap();
        let todo_child = replica.create("carry").unwrap();
        let done_grandchild = replica.create("rinse").unwrap();
        let deleted_child = replica.create("old").unwrap();
        for child in [done_child, todo_child, deleted_child] {
            replica.set_parent(child, Some(id)).unwrap();
        }
        replica
            .set_parent(done_grandchild, Some(done_child))
            .unwrap();
        for done in [done_child, done_grandchild, deleted_child] {
            replica.complete(done).unwrap();
        }
        replica.delete(deleted_child).unwrap();
        let before = replica.pending_ops().len();

        replica.complete(id).unwrap();

        let reopened: Vec<TaskId> = task_changes(&replica, before)
            .into_iter()
            .filter(|(_, change)| *change == TaskChange::Status(TaskStatus::Todo))
            .map(|(task, _)| task)
            .collect();
        let mut expected = vec![done_child, done_grandchild];
        expected.sort();
        assert_eq!(reopened, expected);
        assert_eq!(
            replica.task(deleted_child).unwrap().status(),
            TaskStatus::Done
        );
    }

    #[test]
    fn completing_a_repeating_task_stops_its_timer() {
        let (mut replica, id) = weekly_task(RecurrenceBasis::Due);
        replica.start(id).unwrap();

        replica.complete(id).unwrap();

        assert!(replica.running_entry().is_none());
    }

    #[test]
    fn recurrence_needs_an_editable_task() {
        let mut replica = replica();
        let id = replica.create("a").unwrap();
        replica.delete(id).unwrap();

        assert_eq!(
            replica.set_recurrence(id, "FREQ=DAILY", RecurrenceBasis::Due),
            Err(CommandError::Deleted(id))
        );
        assert_eq!(replica.clear_recurrence(id), Err(CommandError::Deleted(id)));
    }
}
