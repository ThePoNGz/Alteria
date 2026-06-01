use std::time::Instant;

pub trait SystemClock: Send + Sync {
    /// Returns the current date and time in UTC.
    fn utc_now(&self) -> Instant;
}

pub struct RealSystemClock;

impl SystemClock for RealSystemClock {
    fn utc_now(&self) -> Instant {
        Instant::now()
    }
}

// Zed's `FakeSystemClock` (gated behind `test-support`, backed by `parking_lot`)
// is dropped here: single-user Alteria has no use for it and it was the crate's
// only non-std dependency.
