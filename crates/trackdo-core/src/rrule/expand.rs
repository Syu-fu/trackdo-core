//! Computing the dates a recurrence rule names.
//!
//! Each interval of the rule's frequency is a period. The dates of a period
//! are those every `BY` part admits, which gives the expansions and limits
//! of RFC 5545 at once; `BYSETPOS` then picks among them.

use super::{End, Frequency, RRule, WeekdayNum};
use crate::date::{Date, Weekday, days_from_civil};

/// First and last day number of a period, inclusive.
type Period = (i64, i64);

/// A date with its day number, which scanning keeps count of.
#[derive(Clone, Copy)]
struct Day {
    date: Date,
    number: i64,
}

impl Day {
    fn weekday(self) -> Weekday {
        Weekday::ALL[self.number.rem_euclid(7) as usize]
    }
}

impl RRule {
    /// The dates the rule names when it starts on `start`, in order.
    /// `start` always comes first, as RFC 5545 counts it whether or not it
    /// matches the rule. Ends after the last supported year.
    pub fn occurrences(&self, start: Date) -> impl Iterator<Item = Date> + '_ {
        let until = match self.end {
            End::Until(until) => Some(until),
            End::Never | End::Count(_) => None,
        };
        let count = match self.end {
            End::Count(count) => usize::try_from(count).unwrap_or(usize::MAX),
            End::Never | End::Until(_) => usize::MAX,
        };
        let cycle = self.periods_per_cycle();
        let later = (0u64..)
            .map_while(move |index| self.period(start, index))
            .take_while(move |&(first, _)| until.is_none_or(|until| first <= until.day_number()))
            .map(move |period| self.dates_in(start, period))
            .scan(0u32, move |empty, dates| {
                *empty = if dates.is_empty() { *empty + 1 } else { 0 };
                (*empty <= cycle).then_some(dates)
            })
            .flatten()
            .filter(move |&date| date > start);
        std::iter::once(start)
            .chain(later)
            .take_while(move |&date| until.is_none_or(|until| date <= until))
            .take(count)
    }

    /// The first occurrence after `after`, if any.
    pub fn next_after(&self, start: Date, after: Date) -> Option<Date> {
        self.occurrences(start).find(|&date| date > after)
    }

    /// Periods after which the calendar repeats itself, weekdays and week
    /// numbers included: 400 years are exactly 146,097 days, a whole number
    /// of weeks. The periods a rule visits repeat their positions in that
    /// cycle at least as often, whatever the interval, so once this many
    /// periods in a row name no date, none ever will again.
    const fn periods_per_cycle(&self) -> u32 {
        match self.frequency {
            Frequency::Daily => 146_097,
            Frequency::Weekly => 146_097 / 7,
            Frequency::Monthly => 400 * 12,
            Frequency::Yearly => 400,
        }
    }

    /// The `index`th period, or `None` once it begins past the last
    /// supported day.
    fn period(&self, start: Date, index: u64) -> Option<Period> {
        let step = i64::try_from(index.checked_mul(u64::from(self.interval))?).ok()?;
        let last_day = days_from_civil(9999, 12, 31);
        let period = match self.frequency {
            Frequency::Daily => {
                let day = start.day_number().checked_add(step)?;
                (day, day)
            }
            Frequency::Weekly => {
                let back = (start.weekday().days_from_monday() + 7
                    - self.week_start.days_from_monday())
                    % 7;
                let first = start.day_number() - i64::from(back) + step.checked_mul(7)?;
                (first, first + 6)
            }
            Frequency::Monthly => {
                let months = i64::from(start.year()) * 12 + i64::from(start.month() - 1);
                let months = months.checked_add(step)?;
                let year = months.div_euclid(12);
                let month = months.rem_euclid(12) as u8 + 1;
                let first = days_from_civil(year, month, 1);
                let next = days_from_civil(year + (month == 12) as i64, month % 12 + 1, 1);
                (first, next - 1)
            }
            Frequency::Yearly => {
                let year = i64::from(start.year()).checked_add(step)?;
                (days_from_civil(year, 1, 1), days_from_civil(year, 12, 31))
            }
        };
        (period.0 <= last_day).then_some(period)
    }

    /// The dates of `period` the rule names, in order.
    fn dates_in(&self, start: Date, (first, last): Period) -> Vec<Date> {
        let mut dates = Vec::new();
        let mut number = first.max(Date::FIRST.day_number());
        let mut next = Date::from_day_number(number);
        while let Some(date) = next.filter(|_| number <= last) {
            if !self.month_admits(start, date) {
                number += i64::from(date.days_in_month() - date.day()) + 1;
                next = date.first_of_next_month();
                continue;
            }
            if self.day_admits(start, Day { date, number }) {
                dates.push(date);
            }
            number += 1;
            next = date.next_day();
        }
        if self.by_set_pos.is_empty() {
            return dates;
        }
        let len = dates.len() as i64;
        let mut picked: Vec<Date> = self
            .by_set_pos
            .iter()
            .filter_map(|&position| {
                let index = if position > 0 {
                    i64::from(position) - 1
                } else {
                    len + i64::from(position)
                };
                usize::try_from(index)
                    .ok()
                    .and_then(|index| dates.get(index).copied())
            })
            .collect();
        picked.sort();
        picked.dedup();
        picked
    }

    /// Whether no part names days within a month or year, so the rule takes
    /// them from the start.
    fn no_day_parts(&self) -> bool {
        self.by_week_no.is_empty()
            && self.by_year_day.is_empty()
            && self.by_month_day.is_empty()
            && self.by_day.is_empty()
    }

    /// Whether `BYMONTH`, or the month it defaults to, admits `date`.
    fn month_admits(&self, start: Date, date: Date) -> bool {
        if !self.by_month.is_empty() {
            self.by_month.contains(&date.month())
        } else if self.frequency == Frequency::Yearly && self.no_day_parts() {
            date.month() == start.month()
        } else {
            true
        }
    }

    /// Whether every part naming days, or the default it takes from
    /// `start`, admits `date`.
    fn day_admits(&self, start: Date, day: Day) -> bool {
        let Day { date, .. } = day;
        let default = self.no_day_parts();
        let month_day = if !self.by_month_day.is_empty() {
            let day = i16::from(date.day());
            let from_end = day - i16::from(date.days_in_month()) - 1;
            self.by_month_day
                .iter()
                .any(|&number| i16::from(number) == day || i16::from(number) == from_end)
        } else {
            !default
                || !matches!(self.frequency, Frequency::Monthly | Frequency::Yearly)
                || date.day() == start.day()
        };
        if !month_day {
            return false;
        }
        let weekday = if !self.by_day.is_empty() {
            self.by_day
                .iter()
                .any(|&entry| self.weekday_admits(entry, day))
        } else {
            !default || self.frequency != Frequency::Weekly || day.weekday() == start.weekday()
        };
        if !weekday {
            return false;
        }
        let year_day = self.by_year_day.is_empty() || {
            let ordinal = date.ordinal() as i16;
            let from_end = ordinal - date.days_in_year() as i16 - 1;
            self.by_year_day
                .iter()
                .any(|&number| number == ordinal || number == from_end)
        };
        year_day && (self.by_week_no.is_empty() || self.week_admits(day))
    }

    /// Numbered weekdays count within the month, or within the year for
    /// yearly rules without `BYMONTH`.
    fn weekday_admits(&self, entry: WeekdayNum, day: Day) -> bool {
        let Day { date, .. } = day;
        if day.weekday() != entry.weekday {
            return false;
        }
        let Some(ordinal) = entry.ordinal else {
            return true;
        };
        let within_month = self.frequency == Frequency::Monthly || !self.by_month.is_empty();
        let (index, len) = if within_month {
            (i32::from(date.day()) - 1, i32::from(date.days_in_month()))
        } else {
            (
                i32::from(date.ordinal()) - 1,
                i32::from(date.days_in_year()),
            )
        };
        let ordinal = i32::from(ordinal);
        if ordinal > 0 {
            index / 7 + 1 == ordinal
        } else {
            (len - 1 - index) / 7 + 1 == -ordinal
        }
    }

    /// Week 1 is the first week, starting on `WKST`, with at least four days
    /// of the year. Days early in January may belong to the last week of the
    /// year before, and days late in December to week 1 of the next.
    fn week_admits(&self, day: Day) -> bool {
        let Day { date, number: day } = day;
        let year = i64::from(date.year());
        let this = self.week_one_start(year);
        let next = self.week_one_start(year + 1);
        let (owner, following) = if day < this {
            (self.week_one_start(year - 1), this)
        } else if day >= next {
            (next, self.week_one_start(year + 2))
        } else {
            (this, next)
        };
        let week = (day - owner) / 7 + 1;
        let weeks = (following - owner) / 7;
        self.by_week_no
            .iter()
            .any(|&number| i64::from(number) == week || i64::from(number) == week - weeks - 1)
    }

    /// Day number of the first day of week 1 of `year`: the week holding
    /// January 4th.
    fn week_one_start(&self, year: i64) -> i64 {
        let fourth = days_from_civil(year, 1, 4);
        let back =
            (fourth.rem_euclid(7) - i64::from(self.week_start.days_from_monday())).rem_euclid(7);
        fourth - back
    }
}

#[cfg(test)]
mod tests {
    use super::super::RRule;
    use crate::date::Date;

    fn day(text: &str) -> Date {
        let part = |range: std::ops::Range<usize>| text[range].parse().unwrap();
        Date::new(part(0..4), part(4..6) as u8, part(6..8) as u8).unwrap()
    }

    fn days(texts: &[&str]) -> Vec<Date> {
        texts.iter().map(|text| day(text)).collect()
    }

    /// The first `limit` occurrences of `rule` starting at `start`.
    fn first(rule: &str, start: &str, limit: usize) -> Vec<Date> {
        RRule::parse(rule)
            .unwrap()
            .occurrences(day(start))
            .take(limit)
            .collect()
    }

    // Expected dates are the examples of RFC 5545, section 3.8.5.3.

    #[test]
    fn daily() {
        assert_eq!(
            first("FREQ=DAILY;COUNT=10", "19970902", 20),
            days(&[
                "19970902", "19970903", "19970904", "19970905", "19970906", "19970907", "19970908",
                "19970909", "19970910", "19970911",
            ])
        );
        assert_eq!(
            first("FREQ=DAILY;INTERVAL=2", "19970902", 4),
            days(&["19970902", "19970904", "19970906", "19970908"])
        );
        assert_eq!(
            first("FREQ=DAILY;INTERVAL=10;COUNT=5", "19970902", 20),
            days(&["19970902", "19970912", "19970922", "19971002", "19971012"])
        );
    }

    #[test]
    fn every_day_in_january_for_three_years() {
        let all = first(
            "FREQ=YEARLY;UNTIL=20000131;BYMONTH=1;BYDAY=SU,MO,TU,WE,TH,FR,SA",
            "19980101",
            200,
        );
        assert_eq!(all.len(), 93);
        assert_eq!(all[30], day("19980131"));
        assert_eq!(all[31], day("19990101"));
        assert_eq!(all.last(), Some(&day("20000131")));
        assert_eq!(
            first("FREQ=DAILY;UNTIL=20000131;BYMONTH=1", "19980101", 200),
            all
        );
    }

    #[test]
    fn weekly() {
        assert_eq!(
            first("FREQ=WEEKLY;COUNT=10", "19970902", 20),
            days(&[
                "19970902", "19970909", "19970916", "19970923", "19970930", "19971007", "19971014",
                "19971021", "19971028", "19971104",
            ])
        );
        assert_eq!(
            first("FREQ=WEEKLY;COUNT=10;WKST=SU;BYDAY=TU,TH", "19970902", 20),
            days(&[
                "19970902", "19970904", "19970909", "19970911", "19970916", "19970918", "19970923",
                "19970925", "19970930", "19971002",
            ])
        );
        assert_eq!(
            first(
                "FREQ=WEEKLY;INTERVAL=2;UNTIL=19971224;WKST=SU;BYDAY=MO,WE,FR",
                "19970901",
                40
            ),
            days(&[
                "19970901", "19970903", "19970905", "19970915", "19970917", "19970919", "19970929",
                "19971001", "19971003", "19971013", "19971015", "19971017", "19971027", "19971029",
                "19971031", "19971110", "19971112", "19971114", "19971124", "19971126", "19971128",
                "19971208", "19971210", "19971212", "19971222",
                // The RFC ends at midnight on the 24th; a date UNTIL includes the day.
                "19971224",
            ])
        );
    }

    #[test]
    fn week_start_decides_which_days_share_an_interval() {
        assert_eq!(
            first(
                "FREQ=WEEKLY;INTERVAL=2;COUNT=4;BYDAY=TU,SU;WKST=MO",
                "19970805",
                10
            ),
            days(&["19970805", "19970810", "19970819", "19970824"])
        );
        assert_eq!(
            first(
                "FREQ=WEEKLY;INTERVAL=2;COUNT=4;BYDAY=TU,SU;WKST=SU",
                "19970805",
                10
            ),
            days(&["19970805", "19970817", "19970819", "19970831"])
        );
    }

    #[test]
    fn monthly_by_numbered_weekday() {
        assert_eq!(
            first("FREQ=MONTHLY;COUNT=10;BYDAY=1FR", "19970905", 20),
            days(&[
                "19970905", "19971003", "19971107", "19971205", "19980102", "19980206", "19980306",
                "19980403", "19980501", "19980605",
            ])
        );
        assert_eq!(
            first("FREQ=MONTHLY;COUNT=6;BYDAY=-2MO", "19970922", 20),
            days(&[
                "19970922", "19971020", "19971117", "19971222", "19980119", "19980216",
            ])
        );
    }

    #[test]
    fn monthly_by_month_day() {
        assert_eq!(
            first("FREQ=MONTHLY;BYMONTHDAY=-3", "19970928", 6),
            days(&[
                "19970928", "19971029", "19971128", "19971229", "19980129", "19980226",
            ])
        );
        assert_eq!(
            first("FREQ=MONTHLY;COUNT=10;BYMONTHDAY=2,15", "19970902", 20),
            days(&[
                "19970902", "19970915", "19971002", "19971015", "19971102", "19971115", "19971202",
                "19971215", "19980102", "19980115",
            ])
        );
        assert_eq!(
            first("FREQ=MONTHLY;COUNT=10;BYMONTHDAY=1,-1", "19970930", 20),
            days(&[
                "19970930", "19971001", "19971031", "19971101", "19971130", "19971201", "19971231",
                "19980101", "19980131", "19980201",
            ])
        );
    }

    #[test]
    fn every_tuesday_every_other_month() {
        assert_eq!(
            first("FREQ=MONTHLY;INTERVAL=2;BYDAY=TU", "19970902", 10),
            days(&[
                "19970902", "19970909", "19970916", "19970923", "19970930", "19971104", "19971111",
                "19971118", "19971125", "19980106",
            ])
        );
    }

    #[test]
    fn yearly() {
        assert_eq!(
            first("FREQ=YEARLY;COUNT=10;BYMONTH=6,7", "19970610", 20),
            days(&[
                "19970610", "19970710", "19980610", "19980710", "19990610", "19990710", "20000610",
                "20000710", "20010610", "20010710",
            ])
        );
        assert_eq!(
            first(
                "FREQ=YEARLY;INTERVAL=3;COUNT=10;BYYEARDAY=1,100,200",
                "19970101",
                20
            ),
            days(&[
                "19970101", "19970410", "19970719", "20000101", "20000409", "20000718", "20030101",
                "20030410", "20030719", "20060101",
            ])
        );
        assert_eq!(
            first("FREQ=YEARLY;BYDAY=20MO", "19970519", 3),
            days(&["19970519", "19980518", "19990517"])
        );
        assert_eq!(
            first("FREQ=YEARLY;BYWEEKNO=20;BYDAY=MO", "19970512", 3),
            days(&["19970512", "19980511", "19990517"])
        );
        assert_eq!(
            first("FREQ=YEARLY;BYMONTH=3;BYDAY=TH", "19970313", 7),
            days(&[
                "19970313", "19970320", "19970327", "19980305", "19980312", "19980319", "19980326",
            ])
        );
    }

    #[test]
    fn combined_limits() {
        // RFC 5545 excludes the start with EXDATE, as it always counts.
        assert_eq!(
            first("FREQ=MONTHLY;BYDAY=FR;BYMONTHDAY=13", "19970902", 6),
            days(&[
                "19970902", "19980213", "19980313", "19981113", "19990813", "20001013",
            ])
        );
        assert_eq!(
            first(
                "FREQ=MONTHLY;BYDAY=SA;BYMONTHDAY=7,8,9,10,11,12,13",
                "19970913",
                10
            ),
            days(&[
                "19970913", "19971011", "19971108", "19971213", "19980110", "19980207", "19980307",
                "19980411", "19980509", "19980613",
            ])
        );
        assert_eq!(
            first(
                "FREQ=YEARLY;INTERVAL=4;BYMONTH=11;BYDAY=TU;BYMONTHDAY=2,3,4,5,6,7,8",
                "19961105",
                3
            ),
            days(&["19961105", "20001107", "20041102"])
        );
    }

    #[test]
    fn plain_and_numbered_weekdays_each_name_dates() {
        // Every Wednesday and the last Friday of each month.
        assert_eq!(
            first("FREQ=MONTHLY;BYDAY=WE,-1FR", "20261104", 7),
            days(&[
                "20261104", "20261111", "20261118", "20261125", "20261127", "20261202", "20261209",
            ])
        );
    }

    #[test]
    fn set_positions() {
        assert_eq!(
            first(
                "FREQ=MONTHLY;COUNT=3;BYDAY=TU,WE,TH;BYSETPOS=3",
                "19970904",
                10
            ),
            days(&["19970904", "19971007", "19971106"])
        );
        assert_eq!(
            first(
                "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-2",
                "19970929",
                7
            ),
            days(&[
                "19970929", "19971030", "19971127", "19971230", "19980129", "19980226", "19980330",
            ])
        );
    }

    #[test]
    fn days_that_do_not_exist_are_skipped() {
        assert_eq!(
            first("FREQ=MONTHLY;BYMONTHDAY=15,30;COUNT=5", "20070115", 10),
            days(&["20070115", "20070130", "20070215", "20070315", "20070330"])
        );
        assert_eq!(
            first("FREQ=MONTHLY", "20260131", 3),
            days(&["20260131", "20260331", "20260531"])
        );
    }

    #[test]
    fn rules_that_never_match_end_at_the_last_supported_year() {
        assert_eq!(
            first("FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=30", "20260101", 3),
            days(&["20260101"])
        );
    }

    #[test]
    fn long_gaps_between_occurrences_are_kept() {
        // 2100, 2200 and 2300 are not leap years.
        assert_eq!(
            first(
                "FREQ=YEARLY;INTERVAL=100;BYMONTH=2;BYMONTHDAY=29",
                "20000229",
                2
            ),
            days(&["20000229", "24000229"])
        );
        assert_eq!(
            first("FREQ=DAILY;BYMONTH=2;BYMONTHDAY=29", "20970301", 2),
            days(&["20970301", "21040229"])
        );
    }

    #[test]
    fn until_before_the_start_names_no_date() {
        assert_eq!(
            first("FREQ=DAILY;UNTIL=20260101", "20260102", 3),
            Vec::new()
        );
    }

    #[test]
    fn next_after_skips_to_the_first_later_occurrence() {
        let rule = RRule::parse("FREQ=WEEKLY;BYDAY=MO").unwrap();
        let start = day("20261102");

        assert_eq!(
            rule.next_after(start, day("20261102")),
            Some(day("20261109"))
        );
        assert_eq!(
            rule.next_after(start, day("20261105")),
            Some(day("20261109"))
        );
        assert_eq!(rule.next_after(start, day("20261020")), Some(start));
        let counted = RRule::parse("FREQ=WEEKLY;COUNT=2").unwrap();
        assert_eq!(counted.next_after(start, day("20261109")), None);
    }
}
