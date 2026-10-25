//! Calendar dates without a time zone.

use thiserror::Error;

/// A day in the proleptic Gregorian calendar, between years 1 and 9999.
///
/// It names the same day wherever the device is, so a task due on the 25th
/// stays due on the 25th after travelling. Ordering is chronological.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    year: u16,
    month: u8,
    day: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{year:04}-{month:02}-{day:02} is not a valid date")]
pub struct DateError {
    pub year: u16,
    pub month: u8,
    pub day: u8,
}

impl Date {
    pub fn new(year: u16, month: u8, day: u8) -> Result<Self, DateError> {
        let valid = (1..=9999).contains(&year)
            && (1..=12).contains(&month)
            && (1..=days_in_month(year, month)).contains(&day);
        if valid {
            Ok(Self { year, month, day })
        } else {
            Err(DateError { year, month, day })
        }
    }

    pub const fn year(self) -> u16 {
        self.year
    }

    pub const fn month(self) -> u8 {
        self.month
    }

    pub const fn day(self) -> u8 {
        self.day
    }
}

const fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        2 if is_leap_year(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

const fn is_leap_year(year: u16) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invalid(year: u16, month: u8, day: u8) -> Result<Date, DateError> {
        Err(DateError { year, month, day })
    }

    #[test]
    fn exposes_its_parts() {
        let date = Date::new(2026, 10, 25).unwrap();

        assert_eq!((date.year(), date.month(), date.day()), (2026, 10, 25));
    }

    #[test]
    fn accepts_last_day_of_each_month() {
        let last_days = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        for (month, day) in (1..=12).zip(last_days) {
            assert!(Date::new(2026, month, day).is_ok(), "2026-{month}-{day}");
            assert_eq!(
                Date::new(2026, month, day + 1),
                invalid(2026, month, day + 1)
            );
        }
    }

    #[test]
    fn february_29_exists_only_in_leap_years() {
        assert!(Date::new(2024, 2, 29).is_ok());
        assert!(Date::new(2000, 2, 29).is_ok());
        assert_eq!(Date::new(2026, 2, 29), invalid(2026, 2, 29));
        assert_eq!(Date::new(1900, 2, 29), invalid(1900, 2, 29));
    }

    #[test]
    fn rejects_zero_day_and_month_out_of_range() {
        assert_eq!(Date::new(2026, 1, 0), invalid(2026, 1, 0));
        assert_eq!(Date::new(2026, 0, 1), invalid(2026, 0, 1));
        assert_eq!(Date::new(2026, 13, 1), invalid(2026, 13, 1));
    }

    #[test]
    fn year_must_have_four_digits() {
        assert!(Date::new(1, 1, 1).is_ok());
        assert!(Date::new(9999, 12, 31).is_ok());
        assert_eq!(Date::new(0, 1, 1), invalid(0, 1, 1));
        assert_eq!(Date::new(10_000, 1, 1), invalid(10_000, 1, 1));
    }

    #[test]
    fn orders_chronologically() {
        let dates = [(2025, 12, 31), (2026, 1, 1), (2026, 1, 2), (2026, 2, 1)]
            .map(|(y, m, d)| Date::new(y, m, d).unwrap());

        assert!(dates.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
