//! How a task repeats: a rule, its first occurrence, and what the next one is
//! counted from.

use thiserror::Error;

use crate::date::Date;
use crate::rrule::RRule;

/// What the occurrence after a completion is counted from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RecurrenceBasis {
    /// The rule's own dates from its start, such as every Monday for taking
    /// out the bins, however late the task was done.
    Due,
    /// The day the task was done, such as two weeks after the last haircut.
    Completion,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Recurrence {
    rule: RRule,
    start: Date,
    basis: RecurrenceBasis,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RecurrenceError {
    #[error("COUNT needs occurrences counted from the due date")]
    CountFromCompletion,
}

impl Recurrence {
    /// `start` is the first occurrence. A rule with `COUNT` must use
    /// [`RecurrenceBasis::Due`], since counting from each completion has no
    /// fixed series to count.
    pub fn new(rule: RRule, start: Date, basis: RecurrenceBasis) -> Result<Self, RecurrenceError> {
        if rule.has_count() && basis == RecurrenceBasis::Completion {
            return Err(RecurrenceError::CountFromCompletion);
        }
        Ok(Self { rule, start, basis })
    }

    pub const fn rule(&self) -> &RRule {
        &self.rule
    }

    pub const fn start(&self) -> Date {
        self.start
    }

    pub const fn basis(&self) -> RecurrenceBasis {
        self.basis
    }

    /// The due date once the task due on `due` is done on `today`, or `None`
    /// when the rule has ended.
    ///
    /// From the due date, it is the first occurrence after `due` that is not
    /// past, so late completions skip the occurrences missed. From
    /// completion, it is the occurrence after `today` in the series that
    /// starts on `today`.
    pub fn next_due(&self, due: Option<Date>, today: Date) -> Option<Date> {
        match self.basis {
            RecurrenceBasis::Due => self
                .rule
                .occurrences(self.start)
                .find(|&date| date >= today && due.is_none_or(|due| date > due)),
            RecurrenceBasis::Completion => self.rule.next_after(today, today),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(month: u8, day: u8) -> Date {
        Date::new(2026, month, day).unwrap()
    }

    fn recurrence(rule: &str, start: Date, basis: RecurrenceBasis) -> Recurrence {
        Recurrence::new(RRule::parse(rule).unwrap(), start, basis).unwrap()
    }

    /// Every Monday from Monday, November 2nd.
    fn mondays(basis: RecurrenceBasis) -> Recurrence {
        recurrence("FREQ=WEEKLY;BYDAY=MO", day(11, 2), basis)
    }

    #[test]
    fn exposes_its_parts() {
        let weekly = mondays(RecurrenceBasis::Due);

        assert_eq!(
            weekly.rule(),
            &RRule::parse("FREQ=WEEKLY;BYDAY=MO").unwrap()
        );
        assert_eq!(weekly.start(), day(11, 2));
        assert_eq!(weekly.basis(), RecurrenceBasis::Due);
    }

    #[test]
    fn count_needs_the_due_basis() {
        let counted = RRule::parse("FREQ=DAILY;COUNT=3").unwrap();

        assert_eq!(
            Recurrence::new(counted.clone(), day(11, 2), RecurrenceBasis::Completion),
            Err(RecurrenceError::CountFromCompletion)
        );
        assert!(Recurrence::new(counted, day(11, 2), RecurrenceBasis::Due).is_ok());
    }

    #[test]
    fn due_basis_advances_to_the_occurrence_after_the_due_date() {
        let weekly = mondays(RecurrenceBasis::Due);

        // Completed on time or early.
        assert_eq!(
            weekly.next_due(Some(day(11, 2)), day(11, 2)),
            Some(day(11, 9))
        );
        assert_eq!(
            weekly.next_due(Some(day(11, 9)), day(11, 4)),
            Some(day(11, 16))
        );
    }

    #[test]
    fn due_basis_skips_occurrences_already_past() {
        let weekly = mondays(RecurrenceBasis::Due);

        assert_eq!(
            weekly.next_due(Some(day(11, 2)), day(11, 18)),
            Some(day(11, 23))
        );
        // Today's occurrence is still to be done.
        assert_eq!(
            weekly.next_due(Some(day(11, 2)), day(11, 16)),
            Some(day(11, 16))
        );
    }

    #[test]
    fn due_basis_without_a_due_date_takes_the_next_occurrence_from_today() {
        let weekly = mondays(RecurrenceBasis::Due);

        assert_eq!(weekly.next_due(None, day(11, 4)), Some(day(11, 9)));
        assert_eq!(weekly.next_due(None, day(11, 9)), Some(day(11, 9)));
    }

    #[test]
    fn completion_basis_counts_from_today() {
        let every_three_days = recurrence(
            "FREQ=DAILY;INTERVAL=3",
            day(11, 2),
            RecurrenceBasis::Completion,
        );

        assert_eq!(
            every_three_days.next_due(Some(day(11, 2)), day(11, 10)),
            Some(day(11, 13))
        );
        assert_eq!(
            every_three_days.next_due(None, day(11, 1)),
            Some(day(11, 4))
        );
        assert_eq!(
            mondays(RecurrenceBasis::Completion).next_due(Some(day(11, 2)), day(11, 9)),
            Some(day(11, 16))
        );
    }

    #[test]
    fn ended_rules_have_no_next_due_date() {
        let twice = recurrence("FREQ=WEEKLY;COUNT=2", day(11, 2), RecurrenceBasis::Due);
        let until = recurrence(
            "FREQ=DAILY;UNTIL=20261110",
            day(11, 2),
            RecurrenceBasis::Completion,
        );

        assert_eq!(
            twice.next_due(Some(day(11, 2)), day(11, 2)),
            Some(day(11, 9))
        );
        assert_eq!(twice.next_due(Some(day(11, 9)), day(11, 9)), None);
        assert_eq!(
            until.next_due(Some(day(11, 9)), day(11, 9)),
            Some(day(11, 10))
        );
        assert_eq!(until.next_due(Some(day(11, 10)), day(11, 10)), None);
    }
}
