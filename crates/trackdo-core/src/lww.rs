//! Last-writer-wins register.
//!
//! `merge` is a semilattice join: commutative, associative and idempotent,
//! so replicas converge regardless of the order or repetition of updates.

use crate::hlc::Hlc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lww<T> {
    value: T,
    hlc: Hlc,
}

impl<T: Clone + Ord> Lww<T> {
    pub const fn new(value: T, hlc: Hlc) -> Self {
        Self { value, hlc }
    }

    pub const fn value(&self) -> &T {
        &self.value
    }

    pub const fn hlc(&self) -> Hlc {
        self.hlc
    }

    /// The newer of the two registers. Identical timestamps fall back to
    /// comparing values so the result never depends on argument order.
    pub fn merge(&self, other: &Self) -> Self {
        let self_key = (self.hlc, &self.value);
        let other_key = (other.hlc, &other.value);
        if other_key > self_key {
            other.clone()
        } else {
            self.clone()
        }
    }

    /// Register after applying a write of `value` at `hlc`.
    pub fn set(&self, value: T, hlc: Hlc) -> Self {
        self.merge(&Self::new(value, hlc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hlc::DeviceId;
    use proptest::prelude::*;

    const A: DeviceId = DeviceId(1);
    const B: DeviceId = DeviceId(2);

    #[test]
    fn merge_keeps_value_with_newer_timestamp() {
        let older = Lww::new("old", Hlc::new(100, 0, A));
        let newer = Lww::new("new", Hlc::new(200, 0, B));

        assert_eq!(older.merge(&newer), newer);
        assert_eq!(newer.merge(&older), newer);
    }

    #[test]
    fn merge_uses_device_to_break_wall_and_counter_ties() {
        let from_a = Lww::new("a", Hlc::new(100, 0, A));
        let from_b = Lww::new("b", Hlc::new(100, 0, B));

        assert_eq!(from_a.merge(&from_b), from_b);
    }

    #[test]
    fn merge_uses_value_to_break_identical_timestamps() {
        let hlc = Hlc::new(100, 0, A);
        let small = Lww::new("x", hlc);
        let large = Lww::new("y", hlc);

        assert_eq!(small.merge(&large), large);
        assert_eq!(large.merge(&small), large);
    }

    #[test]
    fn set_ignores_stale_write() {
        let current = Lww::new("current", Hlc::new(200, 0, A));

        let after = current.set("stale", Hlc::new(100, 0, B));

        assert_eq!(after, current);
    }

    #[test]
    fn set_applies_newer_write() {
        let current = Lww::new("current", Hlc::new(100, 0, A));

        let after = current.set("next", Hlc::new(200, 0, B));

        assert_eq!(after.value(), &"next");
        assert_eq!(after.hlc(), Hlc::new(200, 0, B));
    }

    // Small domains so that timestamp ties are actually exercised.
    fn register() -> impl Strategy<Value = Lww<u8>> {
        (0u64..3, 0u32..3, 0u64..3, 0u8..3).prop_map(|(wall, counter, device, value)| {
            Lww::new(value, Hlc::new(wall, counter, DeviceId(device)))
        })
    }

    proptest! {
        #[test]
        fn merge_is_commutative(a in register(), b in register()) {
            prop_assert_eq!(a.merge(&b), b.merge(&a));
        }

        #[test]
        fn merge_is_associative(a in register(), b in register(), c in register()) {
            prop_assert_eq!(a.merge(&b).merge(&c), a.merge(&b.merge(&c)));
        }

        #[test]
        fn merge_is_idempotent(a in register(), b in register()) {
            prop_assert_eq!(a.merge(&a), a.clone());
            prop_assert_eq!(a.merge(&b).merge(&b), a.merge(&b));
        }

        #[test]
        fn merge_result_is_one_of_the_inputs(a in register(), b in register()) {
            let merged = a.merge(&b);

            prop_assert!(merged == a || merged == b);
            prop_assert_eq!(merged.hlc(), a.hlc().max(b.hlc()));
        }
    }
}
