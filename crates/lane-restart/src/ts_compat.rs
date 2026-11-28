// SPDX-License-Identifier: MIT OR Apache-2.0

//! O10, lane-state stage: lane-state's instants are `lane_state::timestamp::Timestamp`, while the rest of this
//! crate still holds `chrono` values until its own stage. These two conversions are the only bridge; they go
//! when lane-restart leaves chrono.

use chrono::{DateTime, TimeZone, Utc};
use lane_state::timestamp::Timestamp;

/// `Timestamp` to `chrono`, to the nanosecond.
pub trait ToChrono {
    fn to_chrono(self) -> DateTime<Utc>;
}

impl ToChrono for Timestamp {
    fn to_chrono(self) -> DateTime<Utc> {
        // `Timestamp` truncates toward zero (negative seconds carry negative nanoseconds); chrono floors.
        let (mut secs, mut nanos) = (self.unix_secs(), self.subsec_nanos());
        if nanos < 0 {
            secs -= 1;
            nanos += 1_000_000_000;
        }
        Utc.timestamp_opt(secs, nanos.unsigned_abs())
            .single()
            .unwrap_or_default()
    }
}

/// `chrono` to `Timestamp`, to the nanosecond.
pub trait ToTimestamp {
    fn to_ts(self) -> Timestamp;
}

impl ToTimestamp for DateTime<Utc> {
    fn to_ts(self) -> Timestamp {
        let (secs, nanos) = (self.timestamp(), self.timestamp_subsec_nanos());
        // chrono floors; `Timestamp` truncates toward zero, so a negative instant carries negative nanoseconds.
        let (secs, nanos) = if secs < 0 && nanos > 0 {
            (secs + 1, i64::from(nanos) - 1_000_000_000)
        } else {
            (secs, i64::from(nanos))
        };
        Timestamp::from_unix(secs, i32::try_from(nanos).unwrap_or(0))
            .unwrap_or_else(|| Timestamp::from_unix_secs(0).expect("epoch is in range"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_round_trip_keeps_every_nanosecond() {
        let t = Utc.timestamp_opt(1_789_722_843, 123_456_789).unwrap();
        assert_eq!(t.to_ts().to_chrono(), t);
        assert_eq!(t.to_ts().subsec_nanos(), 123_456_789);
        let neg = Utc.timestamp_opt(-5, 250_000_000).unwrap();
        assert_eq!(neg.to_ts().to_chrono(), neg);
    }
}
