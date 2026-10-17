//! Hybrid Logical Clock.
//!
//! Timestamps are totally ordered by `(wall_ms, counter, device)`, so two
//! replicas always agree on which of two writes is newer even when their
//! physical clocks disagree.

use thiserror::Error;

/// Identifies the replica that produced a timestamp. Breaks ties between
/// timestamps with identical `wall_ms` and `counter`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hlc {
    wall_ms: u64,
    counter: u32,
    device: DeviceId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum HlcError {
    #[error("HLC counter overflowed")]
    CounterOverflow,
}

impl Hlc {
    pub const fn new(wall_ms: u64, counter: u32, device: DeviceId) -> Self {
        Self {
            wall_ms,
            counter,
            device,
        }
    }

    /// The smallest timestamp for `device`.
    pub const fn zero(device: DeviceId) -> Self {
        Self::new(0, 0, device)
    }

    pub const fn wall_ms(&self) -> u64 {
        self.wall_ms
    }

    pub const fn counter(&self) -> u32 {
        self.counter
    }

    pub const fn device(&self) -> DeviceId {
        self.device
    }

    /// Timestamp for a local event, strictly greater than `self`.
    pub fn tick(&self, now_ms: u64) -> Result<Self, HlcError> {
        if now_ms > self.wall_ms {
            return Ok(Self::new(now_ms, 0, self.device));
        }
        Ok(Self::new(self.wall_ms, next(self.counter)?, self.device))
    }

    /// Timestamp after observing `remote`, strictly greater than both
    /// `self` and `remote`. Keeps `self`'s device.
    pub fn receive(&self, remote: &Hlc, now_ms: u64) -> Result<Self, HlcError> {
        let wall_ms = self.wall_ms.max(remote.wall_ms).max(now_ms);
        let local_wins = wall_ms == self.wall_ms;
        let remote_wins = wall_ms == remote.wall_ms;
        let counter = match (local_wins, remote_wins) {
            (true, true) => next(self.counter.max(remote.counter))?,
            (true, false) => next(self.counter)?,
            (false, true) => next(remote.counter)?,
            (false, false) => 0,
        };
        Ok(Self::new(wall_ms, counter, self.device))
    }
}

fn next(counter: u32) -> Result<u32, HlcError> {
    counter.checked_add(1).ok_or(HlcError::CounterOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const A: DeviceId = DeviceId(1);
    const B: DeviceId = DeviceId(2);

    #[test]
    fn tick_adopts_physical_time_when_it_is_ahead() {
        let clock = Hlc::new(100, 5, A);

        let next = clock.tick(200).unwrap();

        assert_eq!(next, Hlc::new(200, 0, A));
    }

    #[test]
    fn tick_increments_counter_within_same_millisecond() {
        let clock = Hlc::new(100, 5, A);

        let next = clock.tick(100).unwrap();

        assert_eq!(next, Hlc::new(100, 6, A));
    }

    #[test]
    fn tick_does_not_go_backwards_when_physical_clock_regresses() {
        let clock = Hlc::new(100, 5, A);

        let next = clock.tick(50).unwrap();

        assert_eq!(next, Hlc::new(100, 6, A));
    }

    #[test]
    fn tick_fails_on_counter_overflow() {
        let clock = Hlc::new(100, u32::MAX, A);

        assert_eq!(clock.tick(100), Err(HlcError::CounterOverflow));
    }

    #[test]
    fn receive_adopts_physical_time_when_it_is_ahead_of_both() {
        let local = Hlc::new(100, 3, A);
        let remote = Hlc::new(150, 7, B);

        let next = local.receive(&remote, 200).unwrap();

        assert_eq!(next, Hlc::new(200, 0, A));
    }

    #[test]
    fn receive_jumps_to_remote_time_when_remote_is_ahead() {
        let local = Hlc::new(100, 3, A);
        let remote = Hlc::new(300, 7, B);

        let next = local.receive(&remote, 200).unwrap();

        assert_eq!(next, Hlc::new(300, 8, A));
    }

    #[test]
    fn receive_increments_local_counter_when_local_is_ahead() {
        let local = Hlc::new(300, 3, A);
        let remote = Hlc::new(100, 7, B);

        let next = local.receive(&remote, 200).unwrap();

        assert_eq!(next, Hlc::new(300, 4, A));
    }

    #[test]
    fn receive_uses_max_counter_when_wall_times_tie() {
        let local = Hlc::new(300, 3, A);
        let remote = Hlc::new(300, 7, B);

        let next = local.receive(&remote, 200).unwrap();

        assert_eq!(next, Hlc::new(300, 8, A));
    }

    #[test]
    fn receive_fails_on_counter_overflow() {
        let local = Hlc::new(300, 0, A);
        let remote = Hlc::new(300, u32::MAX, B);

        assert_eq!(local.receive(&remote, 200), Err(HlcError::CounterOverflow));
    }

    #[test]
    fn device_breaks_ties_between_otherwise_equal_timestamps() {
        assert!(Hlc::new(100, 1, A) < Hlc::new(100, 1, B));
    }

    #[test]
    fn wall_time_dominates_counter_in_ordering() {
        assert!(Hlc::new(100, 99, B) < Hlc::new(101, 0, A));
    }

    fn hlc() -> impl Strategy<Value = Hlc> {
        (0u64..1_000, 0u32..1_000, 0u64..4)
            .prop_map(|(wall, counter, device)| Hlc::new(wall, counter, DeviceId(device)))
    }

    proptest! {
        #[test]
        fn tick_is_strictly_monotonic(clock in hlc(), now in 0u64..2_000) {
            let next = clock.tick(now).unwrap();

            prop_assert!(next > clock);
            prop_assert!(next.wall_ms() >= now);
            prop_assert_eq!(next.device(), clock.device());
        }

        #[test]
        fn receive_exceeds_both_inputs(local in hlc(), remote in hlc(), now in 0u64..2_000) {
            let next = local.receive(&remote, now).unwrap();

            prop_assert!(next > local);
            prop_assert!((next.wall_ms(), next.counter()) > (remote.wall_ms(), remote.counter()));
            prop_assert!(next.wall_ms() >= now);
            prop_assert_eq!(next.device(), local.device());
        }
    }
}
