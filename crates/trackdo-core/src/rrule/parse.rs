//! Reading recurrence rules from their text form.

use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::str::FromStr;

use super::{End, Frequency, RRule, RRuleError, WEEKDAY_CODES, WeekdayNum};
use crate::date::{Date, Weekday};

const PARTS: [&str; 11] = [
    "FREQ",
    "INTERVAL",
    "COUNT",
    "UNTIL",
    "BYMONTH",
    "BYWEEKNO",
    "BYYEARDAY",
    "BYMONTHDAY",
    "BYDAY",
    "BYSETPOS",
    "WKST",
];

const TIME_OF_DAY_PARTS: [&str; 3] = ["BYHOUR", "BYMINUTE", "BYSECOND"];

impl RRule {
    /// Reads a rule such as `FREQ=WEEKLY;BYDAY=MO,TH`, with or without the
    /// `RRULE:` prefix. Names and values are case-insensitive.
    pub fn parse(input: &str) -> Result<Self, RRuleError> {
        let upper = input.to_ascii_uppercase();
        let parts = split_parts(upper.strip_prefix("RRULE:").unwrap_or(&upper))?;
        let frequency = parts
            .get("FREQ")
            .ok_or(RRuleError::MissingFrequency)
            .and_then(|value| frequency(value))?;
        let rule = Self {
            frequency,
            interval: parts
                .get("INTERVAL")
                .map_or(Ok(1), |value| number("INTERVAL", value, 1..=u32::MAX))?,
            end: end(&parts)?,
            by_month: list(&parts, "BYMONTH", |v| number("BYMONTH", v, 1..=12))?,
            by_week_no: list(&parts, "BYWEEKNO", |v| signed("BYWEEKNO", v, 53))?,
            by_year_day: list(&parts, "BYYEARDAY", |v| signed("BYYEARDAY", v, 366))?,
            by_month_day: list(&parts, "BYMONTHDAY", |v| signed("BYMONTHDAY", v, 31))?,
            by_day: list(&parts, "BYDAY", weekday_num)?,
            by_set_pos: list(&parts, "BYSETPOS", |v| signed("BYSETPOS", v, 366))?,
            week_start: parts
                .get("WKST")
                .map_or(Ok(Weekday::Monday), |value| weekday("WKST", value))?,
        };
        rule.check_combination()?;
        Ok(rule)
    }

    fn check_combination(&self) -> Result<(), RRuleError> {
        let not_allowed = |part| RRuleError::PartNotAllowed {
            part,
            frequency: self.frequency,
        };
        let yearly = self.frequency == Frequency::Yearly;
        if !yearly && !self.by_week_no.is_empty() {
            return Err(not_allowed("BYWEEKNO"));
        }
        if !yearly && !self.by_year_day.is_empty() {
            return Err(not_allowed("BYYEARDAY"));
        }
        if self.frequency == Frequency::Weekly && !self.by_month_day.is_empty() {
            return Err(not_allowed("BYMONTHDAY"));
        }
        let numbered = self.by_day.iter().any(|day| day.ordinal.is_some());
        let numbering_allowed = match self.frequency {
            Frequency::Monthly => true,
            Frequency::Yearly => self.by_week_no.is_empty(),
            Frequency::Daily | Frequency::Weekly => false,
        };
        if numbered && !numbering_allowed {
            return Err(RRuleError::OrdinalNotAllowed);
        }
        let other_by_parts = !(self.by_month.is_empty()
            && self.by_week_no.is_empty()
            && self.by_year_day.is_empty()
            && self.by_month_day.is_empty()
            && self.by_day.is_empty());
        if !self.by_set_pos.is_empty() && !other_by_parts {
            return Err(RRuleError::SetPosWithoutOtherPart);
        }
        Ok(())
    }
}

/// The value of each part by name, checking that every part is known and
/// appears once.
fn split_parts(input: &str) -> Result<BTreeMap<&str, &str>, RRuleError> {
    let mut parts = BTreeMap::new();
    for segment in input.split(';') {
        let (name, value) = segment
            .split_once('=')
            .filter(|(name, _)| !name.is_empty())
            .ok_or(RRuleError::Malformed)?;
        if TIME_OF_DAY_PARTS.contains(&name) {
            return Err(RRuleError::TimeOfDayPart(name.to_owned()));
        }
        if !PARTS.contains(&name) {
            return Err(RRuleError::UnknownPart(name.to_owned()));
        }
        if parts.insert(name, value).is_some() {
            return Err(RRuleError::DuplicatePart(name.to_owned()));
        }
    }
    Ok(parts)
}

fn frequency(value: &str) -> Result<Frequency, RRuleError> {
    match value {
        "DAILY" => Ok(Frequency::Daily),
        "WEEKLY" => Ok(Frequency::Weekly),
        "MONTHLY" => Ok(Frequency::Monthly),
        "YEARLY" => Ok(Frequency::Yearly),
        "HOURLY" | "MINUTELY" | "SECONDLY" => {
            Err(RRuleError::UnsupportedFrequency(value.to_owned()))
        }
        _ => Err(invalid("FREQ", value)),
    }
}

fn end(parts: &BTreeMap<&str, &str>) -> Result<End, RRuleError> {
    match (parts.get("COUNT"), parts.get("UNTIL")) {
        (Some(_), Some(_)) => Err(RRuleError::CountAndUntil),
        (Some(count), None) => Ok(End::Count(number("COUNT", count, 1..=u32::MAX)?)),
        (None, Some(until)) => Ok(End::Until(date("UNTIL", until)?)),
        (None, None) => Ok(End::Never),
    }
}

/// The values of a comma-separated part, sorted and without duplicates.
fn list<T: Ord>(
    parts: &BTreeMap<&str, &str>,
    part: &'static str,
    item: impl Fn(&str) -> Result<T, RRuleError>,
) -> Result<Vec<T>, RRuleError> {
    let Some(value) = parts.get(part) else {
        return Ok(Vec::new());
    };
    let mut items = value
        .split(',')
        .map(|raw| item(raw).map_err(|_| invalid(part, value)))
        .collect::<Result<Vec<_>, _>>()?;
    items.sort();
    items.dedup();
    Ok(items)
}

fn number<T: FromStr + PartialOrd>(
    part: &'static str,
    value: &str,
    range: RangeInclusive<T>,
) -> Result<T, RRuleError> {
    value
        .parse()
        .ok()
        .filter(|number| range.contains(number))
        .ok_or_else(|| invalid(part, value))
}

/// A number in `-max..=max` other than zero.
fn signed<T>(part: &'static str, value: &str, max: T) -> Result<T, RRuleError>
where
    T: FromStr + PartialOrd + Default + std::ops::Neg<Output = T> + Copy,
{
    number(part, value, -max..=max).and_then(|number| {
        if number == T::default() {
            Err(invalid(part, value))
        } else {
            Ok(number)
        }
    })
}

fn weekday_num(value: &str) -> Result<WeekdayNum, RRuleError> {
    let split = value
        .len()
        .checked_sub(2)
        .filter(|&at| value.is_char_boundary(at));
    let (ordinal, code) = split
        .map(|at| value.split_at(at))
        .ok_or_else(|| invalid("BYDAY", value))?;
    let ordinal = match ordinal {
        "" => None,
        raw => Some(signed("BYDAY", raw, 53)?),
    };
    Ok(WeekdayNum {
        weekday: weekday("BYDAY", code)?,
        ordinal,
    })
}

fn weekday(part: &'static str, value: &str) -> Result<Weekday, RRuleError> {
    WEEKDAY_CODES
        .iter()
        .position(|&code| code == value)
        .map(|index| Weekday::ALL[index])
        .ok_or_else(|| invalid(part, value))
}

/// A date in the `YYYYMMDD` form.
fn date(part: &'static str, value: &str) -> Result<Date, RRuleError> {
    if value.len() != 8 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid(part, value));
    }
    let field =
        |range: std::ops::Range<usize>| value[range].parse().map_err(|_| invalid(part, value));
    Date::new(field(0..4)?, field(4..6)? as u8, field(6..8)? as u8)
        .map_err(|_| invalid(part, value))
}

fn invalid(part: &'static str, value: &str) -> RRuleError {
    RRuleError::InvalidValue {
        part,
        value: value.to_owned(),
    }
}
