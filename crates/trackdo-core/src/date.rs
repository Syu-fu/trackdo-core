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
    /// The earliest supported date.
    pub(crate) const FIRST: Self = Self {
        year: 1,
        month: 1,
        day: 1,
    };

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

    pub const fn weekday(self) -> Weekday {
        Weekday::ALL[self.day_number().rem_euclid(7) as usize]
    }

    /// Day of the year, from 1.
    pub const fn ordinal(self) -> u16 {
        const DAYS_BEFORE_MONTH: [u16; 12] =
            [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
        let leap_day = (self.month > 2 && is_leap_year(self.year)) as u16;
        DAYS_BEFORE_MONTH[self.month as usize - 1] + leap_day + self.day as u16
    }

    pub const fn days_in_month(self) -> u8 {
        days_in_month(self.year, self.month)
    }

    pub const fn days_in_year(self) -> u16 {
        if is_leap_year(self.year) { 366 } else { 365 }
    }

    /// The date `days` later, or earlier when negative. `None` past the
    /// supported years.
    pub fn add_days(self, days: i32) -> Option<Self> {
        Self::from_day_number(self.day_number() + i64::from(days))
    }

    /// The day after, without the arithmetic of [`Date::add_days`].
    pub(crate) const fn next_day(self) -> Option<Self> {
        if self.day < self.days_in_month() {
            Some(Self {
                day: self.day + 1,
                ..self
            })
        } else {
            self.first_of_next_month()
        }
    }

    pub(crate) const fn first_of_next_month(self) -> Option<Self> {
        match (self.year, self.month) {
            (9999, 12) => None,
            (year, 12) => Some(Self {
                year: year + 1,
                month: 1,
                day: 1,
            }),
            (year, month) => Some(Self {
                year,
                month: month + 1,
                day: 1,
            }),
        }
    }

    /// Days from `self` to `other`, negative when `other` is earlier.
    pub const fn days_until(self, other: Self) -> i32 {
        // The supported range spans fewer than four million days.
        (other.day_number() - self.day_number()) as i32
    }

    /// Days since 0001-01-01, a Monday.
    pub(crate) const fn day_number(self) -> i64 {
        days_from_civil(self.year as i64, self.month, self.day)
    }

    /// The date [`Date::day_number`] maps to `number`, if supported.
    pub(crate) fn from_day_number(number: i64) -> Option<Self> {
        // Howard Hinnant's civil_from_days.
        let days = number + DAY_NUMBER_OF_0001_01_01;
        let era = days.div_euclid(146_097);
        let day_of_era = days - era * 146_097;
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let shifted_month = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
        let month = if shifted_month < 10 {
            shifted_month + 3
        } else {
            shifted_month - 9
        };
        let year = year_of_era + era * 400 + i64::from(month <= 2);
        let year = u16::try_from(year).ok()?;
        Self::new(year, month as u8, day as u8).ok()
    }
}

/// Days from 0001-01-01 to the given day, which may lie outside the
/// supported years.
pub(crate) const fn days_from_civil(year: i64, month: u8, day: u8) -> i64 {
    // Howard Hinnant's days_from_civil, with years starting in March.
    let year = year - (month <= 2) as i64;
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = month as i64;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - DAY_NUMBER_OF_0001_01_01
}

/// Day number of 0001-01-01 in a count starting at 0000-03-01.
const DAY_NUMBER_OF_0001_01_01: i64 = 306;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl Weekday {
    /// Every weekday, from Monday.
    pub const ALL: [Self; 7] = [
        Self::Monday,
        Self::Tuesday,
        Self::Wednesday,
        Self::Thursday,
        Self::Friday,
        Self::Saturday,
        Self::Sunday,
    ];

    /// 0 for Monday up to 6 for Sunday.
    pub const fn days_from_monday(self) -> u8 {
        self as u8
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
    use proptest::prelude::*;

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

    fn date(year: u16, month: u8, day: u8) -> Date {
        Date::new(year, month, day).unwrap()
    }

    #[test]
    fn knows_its_weekday() {
        assert_eq!(date(2026, 11, 3).weekday(), Weekday::Tuesday);
        assert_eq!(date(2000, 1, 1).weekday(), Weekday::Saturday);
        assert_eq!(date(1, 1, 1).weekday(), Weekday::Monday);
        assert_eq!(date(9999, 12, 31).weekday(), Weekday::Friday);
    }

    #[test]
    fn adds_days_across_months_and_years() {
        assert_eq!(date(2026, 12, 31).add_days(1), Some(date(2027, 1, 1)));
        assert_eq!(date(2024, 2, 28).add_days(1), Some(date(2024, 2, 29)));
        assert_eq!(date(2024, 3, 1).add_days(-1), Some(date(2024, 2, 29)));
        assert_eq!(date(2026, 11, 3).add_days(365), Some(date(2027, 11, 3)));
        assert_eq!(date(2026, 11, 3).add_days(0), Some(date(2026, 11, 3)));
    }

    #[test]
    fn adding_days_past_the_supported_range_gives_none() {
        assert_eq!(date(9999, 12, 31).add_days(1), None);
        assert_eq!(date(1, 1, 1).add_days(-1), None);
    }

    #[test]
    fn counts_days_between_dates() {
        assert_eq!(date(2026, 11, 3).days_until(date(2026, 11, 10)), 7);
        assert_eq!(date(2026, 11, 10).days_until(date(2026, 11, 3)), -7);
        assert_eq!(date(1, 1, 1).days_until(date(9999, 12, 31)), 3_652_058);
    }

    #[test]
    fn knows_its_day_of_the_year() {
        assert_eq!(date(2026, 1, 1).ordinal(), 1);
        assert_eq!(date(2026, 12, 31).ordinal(), 365);
        assert_eq!(date(2024, 12, 31).ordinal(), 366);
        assert_eq!(date(2024, 3, 1).ordinal(), 61);
    }

    #[test]
    fn knows_month_and_year_lengths() {
        assert_eq!(date(2024, 2, 10).days_in_month(), 29);
        assert_eq!(date(2026, 2, 10).days_in_month(), 28);
        assert_eq!(date(2024, 2, 10).days_in_year(), 366);
        assert_eq!(date(2026, 2, 10).days_in_year(), 365);
    }

    proptest! {
        #[test]
        fn adding_days_then_counting_them_round_trips(
            (y, m, d) in (1u16..=9999, 1u8..=12, 1u8..=28),
            days in -400_000i32..400_000,
        ) {
            let start = date(y, m, d);
            if let Some(end) = start.add_days(days) {
                prop_assert_eq!(start.days_until(end), days);
                prop_assert_eq!(end.add_days(-days), Some(start));
            }
        }

        #[test]
        fn next_day_has_the_next_weekday(
            (y, m, d) in (1u16..=9999, 1u8..=12, 1u8..=28),
        ) {
            let today = date(y, m, d);
            let tomorrow = today.add_days(1).unwrap();
            prop_assert_eq!(
                tomorrow.weekday().days_from_monday(),
                (today.weekday().days_from_monday() + 1) % 7
            );
        }
    }

    proptest! {
        #[test]
        fn next_day_agrees_with_adding_a_day(
            (y, m, d) in (1u16..=9999, 1u8..=12, 1u8..=31),
        ) {
            if let Ok(today) = Date::new(y, m, d) {
                prop_assert_eq!(today.next_day(), today.add_days(1));
                prop_assert_eq!(
                    today.first_of_next_month(),
                    today.add_days(i32::from(today.days_in_month() - today.day()) + 1)
                );
            }
        }
    }

    #[test]
    fn orders_chronologically() {
        let dates = [(2025, 12, 31), (2026, 1, 1), (2026, 1, 2), (2026, 2, 1)]
            .map(|(y, m, d)| Date::new(y, m, d).unwrap());

        assert!(dates.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
