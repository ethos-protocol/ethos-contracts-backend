//! Issue #345 — Floors/Caps boundary consistency check.
//!
//! [`floors`](crate::floors) and [`caps`](crate::caps) configure per-beneficiary
//! allocation bounds independently. A floor that sits above its matching cap
//! describes an allocation range that can never be satisfied. Both modules call
//! [`ensure_floor_within_cap`] whenever either bound is set — including on an
//! update that follows an earlier set — and surface the shared
//! [`RangeError::FloorExceedsCap`] on conflict.

/// Error shared by [`floors`](crate::floors) and [`caps`](crate::caps) when a
/// configured boundary pair is internally inconsistent.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RangeError {
    /// The floor for a beneficiary/slice is greater than its cap, so no
    /// allocation amount can satisfy both bounds simultaneously.
    FloorExceedsCap,
}

/// Validate that `floor <= cap` for the same beneficiary/slice.
///
/// A non-positive bound is treated as "unset" and always accepted; the check
/// only fires when both a floor and a cap are configured for the beneficiary.
pub fn ensure_floor_within_cap(floor: i128, cap: i128) -> Result<(), RangeError> {
    if floor > 0 && cap > 0 && floor > cap {
        return Err(RangeError::FloorExceedsCap);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_range_is_accepted() {
        assert_eq!(ensure_floor_within_cap(100, 500), Ok(()));
        assert_eq!(ensure_floor_within_cap(500, 500), Ok(()));
    }

    #[test]
    fn unset_bounds_are_accepted() {
        assert_eq!(ensure_floor_within_cap(0, 500), Ok(()));
        assert_eq!(ensure_floor_within_cap(100, 0), Ok(()));
    }

    #[test]
    fn floor_above_cap_is_rejected() {
        assert_eq!(
            ensure_floor_within_cap(600, 500),
            Err(RangeError::FloorExceedsCap)
        );
    }
}
