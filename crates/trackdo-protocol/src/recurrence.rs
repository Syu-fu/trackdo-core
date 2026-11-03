//! Wire form of task recurrences.

use serde::{Deserialize, Serialize};

use trackdo_core::recurrence::{Recurrence, RecurrenceBasis};
use trackdo_core::rrule::RRule;

use crate::date::{decode_date, encode_date};
use crate::error::ProtocolError;

/// How a task repeats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecurrenceDto {
    /// An RFC 5545 recurrence rule of whole days in canonical form, such as
    /// `FREQ=WEEKLY;BYDAY=MO,TH`, without the `RRULE:` prefix.
    pub rule: String,
    /// The first occurrence, `YYYY-MM-DD`.
    pub start: String,
    pub basis: RecurrenceBasisDto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecurrenceBasisDto {
    Due,
    Completion,
}

impl From<&Recurrence> for RecurrenceDto {
    fn from(recurrence: &Recurrence) -> Self {
        Self {
            rule: recurrence.rule().to_string(),
            start: encode_date(recurrence.start()),
            basis: match recurrence.basis() {
                RecurrenceBasis::Due => RecurrenceBasisDto::Due,
                RecurrenceBasis::Completion => RecurrenceBasisDto::Completion,
            },
        }
    }
}

impl TryFrom<RecurrenceDto> for Recurrence {
    type Error = ProtocolError;

    fn try_from(dto: RecurrenceDto) -> Result<Self, Self::Error> {
        let rule = RRule::parse(&dto.rule)?;
        // Every replica must store the exact same rule.
        if rule.to_string() != dto.rule {
            return Err(ProtocolError::NonCanonicalRecurrenceRule);
        }
        let basis = match dto.basis {
            RecurrenceBasisDto::Due => RecurrenceBasis::Due,
            RecurrenceBasisDto::Completion => RecurrenceBasis::Completion,
        };
        Ok(Self::new(rule, decode_date(&dto.start)?, basis)?)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use proptest::prelude::*;
    use trackdo_core::date::Date;
    use trackdo_core::recurrence::{RecurrenceBasis, RecurrenceError};
    use trackdo_core::rrule::RRule;

    pub(crate) fn weekly() -> Recurrence {
        Recurrence::new(
            RRule::parse("FREQ=WEEKLY;BYDAY=MO,TH").unwrap(),
            Date::new(2026, 11, 2).unwrap(),
            RecurrenceBasis::Due,
        )
        .unwrap()
    }

    fn dto(rule: &str, start: &str, basis: RecurrenceBasisDto) -> RecurrenceDto {
        RecurrenceDto {
            rule: rule.to_owned(),
            start: start.to_owned(),
            basis,
        }
    }

    #[test]
    fn encodes_the_canonical_rule_and_the_start_date() {
        assert_eq!(
            RecurrenceDto::from(&weekly()),
            dto(
                "FREQ=WEEKLY;BYDAY=MO,TH",
                "2026-11-02",
                RecurrenceBasisDto::Due
            )
        );
    }

    #[test]
    fn rejects_rules_not_in_canonical_form() {
        for rule in ["freq=weekly", "FREQ=WEEKLY;BYDAY=TH,MO", "RRULE:FREQ=DAILY"] {
            assert_eq!(
                Recurrence::try_from(dto(rule, "2026-11-02", RecurrenceBasisDto::Due)),
                Err(ProtocolError::NonCanonicalRecurrenceRule),
                "{rule}"
            );
        }
    }

    #[test]
    fn rejects_invalid_rules_and_recurrences() {
        assert!(matches!(
            Recurrence::try_from(dto("FREQ=HOURLY", "2026-11-02", RecurrenceBasisDto::Due)),
            Err(ProtocolError::InvalidRecurrenceRule(_))
        ));
        assert_eq!(
            Recurrence::try_from(dto(
                "FREQ=DAILY;COUNT=3",
                "2026-11-02",
                RecurrenceBasisDto::Completion
            )),
            Err(ProtocolError::InvalidRecurrence(
                RecurrenceError::CountFromCompletion
            ))
        );
        assert_eq!(
            Recurrence::try_from(dto("FREQ=DAILY", "20261102", RecurrenceBasisDto::Due)),
            Err(ProtocolError::InvalidDateFormat)
        );
    }

    pub(crate) fn any_recurrence() -> impl Strategy<Value = Recurrence> {
        (
            prop::sample::select(vec![
                "FREQ=DAILY",
                "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,FR;WKST=SU",
                "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1",
                "FREQ=YEARLY;UNTIL=20301231;BYMONTH=2;BYMONTHDAY=-1",
            ]),
            crate::date::tests::any_date(),
            prop::sample::select(vec![RecurrenceBasis::Due, RecurrenceBasis::Completion]),
        )
            .prop_map(|(rule, start, basis)| {
                Recurrence::new(RRule::parse(rule).unwrap(), start, basis).unwrap()
            })
    }

    proptest! {
        #[test]
        fn json_roundtrip_preserves_recurrence(recurrence in any_recurrence()) {
            let json = serde_json::to_string(&RecurrenceDto::from(&recurrence)).unwrap();
            let dto: RecurrenceDto = serde_json::from_str(&json).unwrap();

            prop_assert_eq!(Recurrence::try_from(dto), Ok(recurrence));
        }
    }
}
