// SPDX-License-Identifier: MIT OR Apache-2.0
//! Time as an injected dependency, so timeouts, month boundaries and expiries are testable
//! without waiting (the technique `lane-restart` uses for its 15-minute liveness path).

use lane_state::timestamp::Timestamp;
use std::sync::Mutex;

pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
}

/// The wall clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// A clock that only moves when told to.
#[derive(Debug)]
pub struct ManualClock(Mutex<Timestamp>);

impl ManualClock {
    pub fn new(at: Timestamp) -> Self {
        Self(Mutex::new(at))
    }

    pub fn set(&self, at: Timestamp) {
        *self.0.lock().expect("manual clock poisoned") = at;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Timestamp {
        *self.0.lock().expect("manual clock poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manual_clock_stays_put_until_set() {
        let t0 = Timestamp::from_utc(2026, 10, 7, 1, 0, 0).unwrap();
        let t1 = Timestamp::from_utc(2026, 11, 1, 0, 0, 0).unwrap();
        let c = ManualClock::new(t0);
        assert_eq!(c.now(), t0);
        assert_eq!(c.now(), t0);
        c.set(t1);
        assert_eq!(c.now(), t1);
    }

    #[test]
    fn the_system_clock_is_not_in_the_past_of_a_fixed_date() {
        let floor = Timestamp::from_utc(2026, 1, 1, 0, 0, 0).unwrap();
        assert!(SystemClock.now() > floor);
    }
}
