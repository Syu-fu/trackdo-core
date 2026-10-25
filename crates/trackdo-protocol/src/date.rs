//! Wire form of calendar dates: `YYYY-MM-DD`.

use trackdo_core::date::Date;

use crate::error::ProtocolError;

pub(crate) fn encode_date(date: Date) -> String {
    format!("{:04}-{:02}-{:02}", date.year(), date.month(), date.day())
}

/// Accepts exactly the form [`encode_date`] produces.
pub(crate) fn decode_date(raw: &str) -> Result<Date, ProtocolError> {
    let well_formed = raw.len() == 10
        && raw.bytes().enumerate().all(|(i, byte)| match i {
            4 | 7 => byte == b'-',
            _ => byte.is_ascii_digit(),
        });
    if !well_formed {
        return Err(ProtocolError::InvalidDateFormat);
    }
    let format_error = |_| ProtocolError::InvalidDateFormat;
    let year = raw[0..4].parse().map_err(format_error)?;
    let month = raw[5..7].parse().map_err(format_error)?;
    let day = raw[8..10].parse().map_err(format_error)?;
    Ok(Date::new(year, month, day)?)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use proptest::prelude::*;

    pub(crate) fn any_date() -> impl Strategy<Value = Date> {
        (1u16..=9999, 1u8..=12, 1u8..=28).prop_map(|(y, m, d)| Date::new(y, m, d).unwrap())
    }

    #[test]
    fn pads_every_part() {
        assert_eq!(encode_date(Date::new(7, 3, 9).unwrap()), "0007-03-09");
        assert_eq!(encode_date(Date::new(2026, 10, 31).unwrap()), "2026-10-31");
    }

    #[test]
    fn decodes_padded_date() {
        assert_eq!(
            decode_date("2024-02-29"),
            Ok(Date::new(2024, 2, 29).unwrap())
        );
    }

    #[test]
    fn rejects_other_forms() {
        for raw in [
            "2026-1-05",
            "26-10-31",
            "2026/10/31",
            "2026-10-31T00:00",
            "+026-10-31",
            "２０２６-10-31",
            "",
        ] {
            assert_eq!(
                decode_date(raw),
                Err(ProtocolError::InvalidDateFormat),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn rejects_dates_that_do_not_exist() {
        assert!(matches!(
            decode_date("2026-02-29"),
            Err(ProtocolError::InvalidDate(_))
        ));
        assert!(matches!(
            decode_date("0000-01-01"),
            Err(ProtocolError::InvalidDate(_))
        ));
    }

    proptest! {
        #[test]
        fn roundtrip_preserves_date(date in any_date()) {
            prop_assert_eq!(decode_date(&encode_date(date)), Ok(date));
        }
    }
}
