//! Time a task is planned to take, to compare with the time spent on it.

use std::time::Duration;

use thiserror::Error;

/// Largest estimate, in minutes: a thousand hours.
pub const MAX_ESTIMATE_MINUTES: u32 = 60_000;

/// A whole number of minutes, from one to [`MAX_ESTIMATE_MINUTES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Estimate(u32);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EstimateError {
    #[error("estimate must be at least one minute")]
    Zero,
    #[error("estimate must be at most {max} minutes, got {actual}")]
    TooLong { max: u32, actual: u32 },
}

impl Estimate {
    pub const fn from_minutes(minutes: u32) -> Result<Self, EstimateError> {
        if minutes == 0 {
            return Err(EstimateError::Zero);
        }
        if minutes > MAX_ESTIMATE_MINUTES {
            return Err(EstimateError::TooLong {
                max: MAX_ESTIMATE_MINUTES,
                actual: minutes,
            });
        }
        Ok(Self(minutes))
    }

    pub const fn minutes(self) -> u32 {
        self.0
    }

    pub const fn as_duration(self) -> Duration {
        Duration::from_secs(self.0 as u64 * 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_minutes() {
        let estimate = Estimate::from_minutes(90).unwrap();

        assert_eq!(estimate.minutes(), 90);
        assert_eq!(estimate.as_duration(), Duration::from_secs(90 * 60));
    }

    #[test]
    fn rejects_zero() {
        assert_eq!(Estimate::from_minutes(0), Err(EstimateError::Zero));
    }

    #[test]
    fn accepts_up_to_the_maximum() {
        assert!(Estimate::from_minutes(MAX_ESTIMATE_MINUTES).is_ok());
        assert_eq!(
            Estimate::from_minutes(MAX_ESTIMATE_MINUTES + 1),
            Err(EstimateError::TooLong {
                max: MAX_ESTIMATE_MINUTES,
                actual: MAX_ESTIMATE_MINUTES + 1,
            })
        );
    }

    #[test]
    fn orders_by_length() {
        let short = Estimate::from_minutes(15).unwrap();
        let long = Estimate::from_minutes(120).unwrap();

        assert!(short < long);
    }
}
