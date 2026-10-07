// SPDX-License-Identifier: MIT OR Apache-2.0
//! Time as an injected dependency, so timeouts, month boundaries and expiries are testable
//! without waiting (the technique `lane-restart` uses for its 15-minute liveness path).

use chrono::{DateTime, Utc};
use std::sync::Mutex;

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// The wall clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A clock that only moves when told to.
#[derive(Debug)]
pub struct ManualClock(Mutex<DateTime<Utc>>);

impl ManualClock {
    pub fn new(at: DateTime<Utc>) -> Self {
        Self(Mutex::new(at))
    }

    pub fn set(&self, at: DateTime<Utc>) {
        *self.0.lock().expect("manual clock poisoned") = at;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().expect("manual clock poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn a_manual_clock_stays_put_until_set() {
        let t0 = Utc.with_ymd_and_hms(2026, 10, 7, 1, 0, 0).unwrap();
        let t1 = Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap();
        let c = ManualClock::new(t0);
        assert_eq!(c.now(), t0);
        assert_eq!(c.now(), t0);
        c.set(t1);
        assert_eq!(c.now(), t1);
    }

    #[test]
    fn the_system_clock_is_not_in_the_past_of_a_fixed_date() {
        let floor = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        assert!(SystemClock.now() > floor);
    }
}
