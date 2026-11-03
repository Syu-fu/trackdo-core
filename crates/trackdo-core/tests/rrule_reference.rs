//! Occurrences must match those python-dateutil computes for random rules,
//! recorded in `data/rrule_reference.txt` by `generate_rrule_reference.py`.

use trackdo_core::date::Date;
use trackdo_core::rrule::RRule;

/// Must match `LIMIT` and `HORIZON` of the generator.
const LIMIT: usize = 12;
const HORIZON: (u16, u8, u8) = (2080, 12, 31);

fn date(text: &str) -> Date {
    let part = |range: std::ops::Range<usize>| text[range].parse::<u16>().unwrap();
    Date::new(part(0..4), part(4..6) as u8, part(6..8) as u8).unwrap()
}

#[test]
fn occurrences_match_python_dateutil() {
    let horizon = Date::new(HORIZON.0, HORIZON.1, HORIZON.2).unwrap();
    let cases = include_str!("data/rrule_reference.txt");
    let mut checked = 0;
    for line in cases.lines() {
        let mut fields = line.split(' ');
        let (start, rule, expected) = (
            date(fields.next().unwrap()),
            fields.next().unwrap(),
            fields.next().unwrap_or(""),
        );
        let expected: Vec<Date> = expected
            .split(',')
            .filter(|d| !d.is_empty())
            .map(date)
            .collect();

        let actual: Vec<Date> = RRule::parse(rule)
            .unwrap_or_else(|error| panic!("{rule}: {error}"))
            .occurrences(start)
            .take_while(|&d| d <= horizon)
            .take(LIMIT)
            .collect();

        assert_eq!(
            actual,
            expected,
            "{} {rule}",
            line.split(' ').next().unwrap()
        );
        checked += 1;
    }
    assert_eq!(checked, 400);
}
