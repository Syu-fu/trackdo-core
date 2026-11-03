//! Commands making tasks repeat.

use super::{CommandError, Replica};
use crate::env::{Clock, IdGen};
use crate::recurrence::{Recurrence, RecurrenceBasis};
use crate::rrule::RRule;
use crate::task::{Task, TaskChange, TaskId};

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
    use crate::recurrence::{Recurrence, RecurrenceBasis, RecurrenceError};
    use crate::rrule::{RRule, RRuleError};
    use crate::task::TaskChange;

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
