//! Recurrence rules of RFC 5545, limited to whole days.

mod parse;

use std::fmt;

use thiserror::Error;

use crate::date::{Date, Weekday};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Frequency {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl Frequency {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "DAILY",
            Self::Weekly => "WEEKLY",
            Self::Monthly => "MONTHLY",
            Self::Yearly => "YEARLY",
        }
    }
}

impl fmt::Display for Frequency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A `BYDAY` entry: a weekday, optionally numbered within the month or
/// year, such as `2TU` for the second Tuesday or `-1FR` for the last Friday.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct WeekdayNum {
    weekday: Weekday,
    /// `None` for every such weekday. Never zero.
    ordinal: Option<i8>,
}

/// When the occurrences end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum End {
    Never,
    /// After this many occurrences, the first included.
    Count(u32),
    /// On or before this date.
    Until(Date),
}

/// A recurrence rule whose occurrences are whole days, so the parts finer
/// than a day are not supported. Lists are kept sorted without duplicates,
/// and [`fmt::Display`] writes the canonical form.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RRule {
    frequency: Frequency,
    interval: u32,
    end: End,
    by_month: Vec<u8>,
    by_week_no: Vec<i8>,
    by_year_day: Vec<i16>,
    by_month_day: Vec<i8>,
    by_day: Vec<WeekdayNum>,
    by_set_pos: Vec<i16>,
    week_start: Weekday,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RRuleError {
    #[error("recurrence rule must be NAME=VALUE parts separated by ';'")]
    Malformed,
    #[error("recurrence rule needs FREQ")]
    MissingFrequency,
    #[error("FREQ={0} repeats more often than daily")]
    UnsupportedFrequency(String),
    #[error("{0} names a time of day, but occurrences are whole days")]
    TimeOfDayPart(String),
    #[error("unknown recurrence rule part {0}")]
    UnknownPart(String),
    #[error("{0} appears more than once")]
    DuplicatePart(String),
    #[error("{part}={value} is not valid")]
    InvalidValue { part: &'static str, value: String },
    #[error("COUNT and UNTIL cannot both be set")]
    CountAndUntil,
    #[error("{part} cannot be used with FREQ={frequency}")]
    PartNotAllowed {
        part: &'static str,
        frequency: Frequency,
    },
    #[error("numbered weekdays need FREQ=MONTHLY, or FREQ=YEARLY without BYWEEKNO")]
    OrdinalNotAllowed,
    #[error("BYSETPOS needs another BY part")]
    SetPosWithoutOtherPart,
}

impl fmt::Display for RRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FREQ={}", self.frequency)?;
        if self.interval != 1 {
            write!(f, ";INTERVAL={}", self.interval)?;
        }
        match self.end {
            End::Never => {}
            End::Count(count) => write!(f, ";COUNT={count}")?,
            End::Until(date) => write!(
                f,
                ";UNTIL={:04}{:02}{:02}",
                date.year(),
                date.month(),
                date.day()
            )?,
        }
        write_list(f, "BYMONTH", &self.by_month)?;
        write_list(f, "BYWEEKNO", &self.by_week_no)?;
        write_list(f, "BYYEARDAY", &self.by_year_day)?;
        write_list(f, "BYMONTHDAY", &self.by_month_day)?;
        write_list(f, "BYDAY", &self.by_day)?;
        write_list(f, "BYSETPOS", &self.by_set_pos)?;
        if self.week_start != Weekday::Monday {
            write!(f, ";WKST={}", weekday_code(self.week_start))?;
        }
        Ok(())
    }
}

impl fmt::Display for WeekdayNum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(ordinal) = self.ordinal {
            write!(f, "{ordinal}")?;
        }
        f.write_str(weekday_code(self.weekday))
    }
}

fn write_list<T: fmt::Display>(
    f: &mut fmt::Formatter<'_>,
    part: &str,
    values: &[T],
) -> fmt::Result {
    let Some((first, rest)) = values.split_first() else {
        return Ok(());
    };
    write!(f, ";{part}={first}")?;
    rest.iter().try_for_each(|value| write!(f, ",{value}"))
}

const WEEKDAY_CODES: [&str; 7] = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"];

const fn weekday_code(weekday: Weekday) -> &'static str {
    WEEKDAY_CODES[weekday.days_from_monday() as usize]
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use proptest::prelude::*;

    fn canonical(input: &str) -> String {
        RRule::parse(input).unwrap().to_string()
    }

    fn error(input: &str) -> RRuleError {
        RRule::parse(input).unwrap_err()
    }

    #[test]
    fn parses_minimal_rule() {
        assert_eq!(canonical("FREQ=DAILY"), "FREQ=DAILY");
    }

    #[test]
    fn canonical_form_orders_parts_and_drops_defaults() {
        assert_eq!(
            canonical("freq=weekly;wkst=mo;interval=1;byday=fr,mo"),
            "FREQ=WEEKLY;BYDAY=MO,FR"
        );
        assert_eq!(
            canonical("BYSETPOS=-1;BYDAY=MO,TU,WE,TH,FR;FREQ=MONTHLY"),
            "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1"
        );
        assert_eq!(
            canonical("FREQ=WEEKLY;WKST=SU;INTERVAL=2;BYDAY=TU"),
            "FREQ=WEEKLY;INTERVAL=2;BYDAY=TU;WKST=SU"
        );
    }

    #[test]
    fn canonical_form_sorts_and_dedups_lists() {
        assert_eq!(
            canonical("FREQ=MONTHLY;BYMONTHDAY=15,-1,1,15"),
            "FREQ=MONTHLY;BYMONTHDAY=-1,1,15"
        );
        assert_eq!(
            canonical("FREQ=MONTHLY;BYDAY=-1FR,+2TU,FR,2TU"),
            "FREQ=MONTHLY;BYDAY=2TU,FR,-1FR"
        );
    }

    #[test]
    fn accepts_the_rrule_prefix() {
        assert_eq!(
            canonical("RRULE:FREQ=YEARLY;BYMONTH=3,1"),
            "FREQ=YEARLY;BYMONTH=1,3"
        );
    }

    #[test]
    fn keeps_every_part() {
        for rule in [
            "FREQ=DAILY;COUNT=10",
            "FREQ=DAILY;UNTIL=20261224",
            "FREQ=YEARLY;INTERVAL=4;BYMONTH=11;BYMONTHDAY=2,3,4,5,6,7,8;BYDAY=TU",
            "FREQ=YEARLY;BYWEEKNO=20;BYDAY=MO",
            "FREQ=YEARLY;BYYEARDAY=-366,1,100,200",
            "FREQ=MONTHLY;BYDAY=TU,WE,TH;BYSETPOS=3",
            "FREQ=YEARLY;BYWEEKNO=-53,1;WKST=SU",
            "FREQ=MONTHLY;BYDAY=53MO,-53SU",
        ] {
            assert_eq!(canonical(rule), rule);
        }
    }

    #[test]
    fn requires_a_supported_frequency() {
        assert_eq!(error(""), RRuleError::Malformed);
        assert_eq!(error("INTERVAL=2"), RRuleError::MissingFrequency);
        assert_eq!(
            error("FREQ=HOURLY"),
            RRuleError::UnsupportedFrequency("HOURLY".to_owned())
        );
        assert_eq!(
            error("FREQ=FORTNIGHTLY"),
            RRuleError::InvalidValue {
                part: "FREQ",
                value: "FORTNIGHTLY".to_owned()
            }
        );
    }

    #[test]
    fn rejects_parts_finer_than_a_day() {
        for part in ["BYHOUR=9", "BYMINUTE=0", "BYSECOND=0"] {
            assert!(matches!(
                error(&format!("FREQ=DAILY;{part}")),
                RRuleError::TimeOfDayPart(_)
            ));
        }
    }

    #[test]
    fn rejects_malformed_parts() {
        assert_eq!(error("FREQ=DAILY;"), RRuleError::Malformed);
        assert_eq!(error("FREQ=DAILY;COUNT"), RRuleError::Malformed);
        assert_eq!(
            error("FREQ=DAILY;X-NAME=1"),
            RRuleError::UnknownPart("X-NAME".to_owned())
        );
        assert_eq!(
            error("FREQ=DAILY;FREQ=WEEKLY"),
            RRuleError::DuplicatePart("FREQ".to_owned())
        );
        assert_eq!(
            error("FREQ=DAILY;COUNT=3;UNTIL=20261231"),
            RRuleError::CountAndUntil
        );
    }

    #[test]
    fn rejects_values_out_of_range() {
        for (part, value) in [
            ("INTERVAL", "0"),
            ("COUNT", "0"),
            ("COUNT", "-1"),
            ("UNTIL", "20261231T000000Z"),
            ("UNTIL", "20260230"),
            ("BYMONTH", "13"),
            ("BYMONTHDAY", "0"),
            ("BYMONTHDAY", "32"),
            ("BYYEARDAY", "367"),
            ("BYWEEKNO", "54"),
            ("BYDAY", "54MO"),
            ("BYDAY", "0MO"),
            ("BYDAY", "MON"),
            ("BYDAY", ""),
            ("BYSETPOS", "0"),
            ("WKST", "1MO"),
        ] {
            let freq = if part == "BYWEEKNO" || part == "BYYEARDAY" {
                "YEARLY"
            } else {
                "MONTHLY"
            };
            let rule = format!("FREQ={freq};{part}={value}");
            assert_eq!(
                error(&rule),
                RRuleError::InvalidValue {
                    part,
                    value: value.to_owned()
                },
                "{rule}"
            );
        }
    }

    #[test]
    fn rejects_parts_the_frequency_does_not_allow() {
        for (rule, part, frequency) in [
            ("FREQ=MONTHLY;BYWEEKNO=1", "BYWEEKNO", Frequency::Monthly),
            ("FREQ=DAILY;BYYEARDAY=1", "BYYEARDAY", Frequency::Daily),
            ("FREQ=WEEKLY;BYYEARDAY=1", "BYYEARDAY", Frequency::Weekly),
            ("FREQ=MONTHLY;BYYEARDAY=1", "BYYEARDAY", Frequency::Monthly),
            ("FREQ=WEEKLY;BYMONTHDAY=1", "BYMONTHDAY", Frequency::Weekly),
        ] {
            assert_eq!(
                error(rule),
                RRuleError::PartNotAllowed { part, frequency },
                "{rule}"
            );
        }
    }

    #[test]
    fn allows_ordinal_weekdays_only_within_months_or_years() {
        assert_eq!(error("FREQ=DAILY;BYDAY=1MO"), RRuleError::OrdinalNotAllowed);
        assert_eq!(
            error("FREQ=WEEKLY;BYDAY=1MO"),
            RRuleError::OrdinalNotAllowed
        );
        assert_eq!(
            error("FREQ=YEARLY;BYWEEKNO=1;BYDAY=1MO"),
            RRuleError::OrdinalNotAllowed
        );
        assert!(RRule::parse("FREQ=YEARLY;BYDAY=20MO").is_ok());
    }

    /// Rule text built from random parts. Parsing rejects the combinations
    /// that are not allowed.
    pub(crate) fn any_rule_text() -> impl Strategy<Value = String> {
        fn list<T: std::fmt::Debug + std::fmt::Display>(
            part: &'static str,
            item: impl Strategy<Value = T>,
        ) -> impl Strategy<Value = String> {
            prop::option::of(prop::collection::vec(item, 1..4)).prop_map(move |items| {
                items.map_or_else(String::new, |items| {
                    let joined: Vec<_> = items.iter().map(ToString::to_string).collect();
                    format!(";{part}={}", joined.join(","))
                })
            })
        }
        let weekday = prop::sample::select(WEEKDAY_CODES.to_vec());
        let by_day = (
            prop::option::of(prop_oneof![-5i8..=-1, 1i8..=5]),
            weekday.clone(),
        )
            .prop_map(|(ordinal, code)| ordinal.map_or_else(String::new, |n| n.to_string()) + code);
        let end = prop_oneof![
            Just(String::new()),
            (1u32..20).prop_map(|count| format!(";COUNT={count}")),
            (2026u16..2030, 1u8..=12, 1u8..=28)
                .prop_map(|(y, m, d)| format!(";UNTIL={y:04}{m:02}{d:02}")),
        ];
        (
            prop::sample::select(vec!["DAILY", "WEEKLY", "MONTHLY", "YEARLY"]),
            prop::option::of(1u32..4),
            end,
            list("BYMONTH", 1u8..=12),
            list("BYWEEKNO", prop_oneof![-53i8..=-1, 1i8..=53]),
            list("BYYEARDAY", prop_oneof![-366i16..=-1, 1i16..=366]),
            list("BYMONTHDAY", prop_oneof![-31i8..=-1, 1i8..=31]),
            list("BYDAY", by_day),
            list("BYSETPOS", prop_oneof![-3i16..=-1, 1i16..=3]),
            prop::option::of(weekday),
        )
            .prop_map(
                |(freq, interval, end, month, week, year_day, month_day, day, set_pos, wkst)| {
                    let interval = interval.map_or_else(String::new, |n| format!(";INTERVAL={n}"));
                    let wkst = wkst.map_or_else(String::new, |code| format!(";WKST={code}"));
                    format!(
                        "FREQ={freq}{interval}{end}{month}{week}{year_day}{month_day}{day}{set_pos}{wkst}"
                    )
                },
            )
    }

    proptest! {
        #[test]
        fn canonical_form_reads_back_as_the_same_rule(text in any_rule_text()) {
            if let Ok(rule) = RRule::parse(&text) {
                let canonical = rule.to_string();
                prop_assert_eq!(RRule::parse(&canonical), Ok(rule));
                prop_assert_eq!(RRule::parse(&canonical).unwrap().to_string(), canonical);
            }
        }
    }

    #[test]
    fn set_position_needs_another_by_part() {
        assert_eq!(
            error("FREQ=MONTHLY;BYSETPOS=1"),
            RRuleError::SetPosWithoutOtherPart
        );
    }
}
