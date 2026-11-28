// SPDX-License-Identifier: MIT OR Apache-2.0

//! `Timestamp`: the one instant type of this workspace, replacing `chrono::DateTime<Utc>`
//! (RUSTSEC-2026-0543: chrono is unmaintained). A newtype over `jiff::Timestamp`, so a
//! consumer never names the backing crate and a later swap stays inside this file.
//!
//! Serde is RFC 3339, the format chrono wrote, so every state file already on disk reads
//! back unchanged.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// An instant on the UTC timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(jiff::Timestamp);

impl Timestamp {
    /// The current instant.
    #[must_use]
    pub fn now() -> Self {
        Self(jiff::Timestamp::now())
    }

    /// The instant `secs` whole seconds after the Unix epoch (negative: before it).
    /// `None` when out of range.
    #[must_use]
    pub fn from_unix_secs(secs: i64) -> Option<Self> {
        jiff::Timestamp::from_second(secs).ok().map(Self)
    }

    /// The instant `secs` seconds and `nanos` nanoseconds after the Unix epoch. `None` when out of range.
    #[must_use]
    pub fn from_unix(secs: i64, nanos: i32) -> Option<Self> {
        jiff::Timestamp::new(secs, nanos).ok().map(Self)
    }

    /// The nanoseconds past the whole second (see [`Timestamp::unix_secs`]).
    #[must_use]
    pub fn subsec_nanos(self) -> i32 {
        self.0.subsec_nanosecond()
    }

    /// Whole seconds since the Unix epoch, rounded toward zero for a fractional instant.
    #[must_use]
    pub fn unix_secs(self) -> i64 {
        self.0.as_second()
    }

    /// `self - earlier` in whole seconds, negative when `earlier` is later.
    #[must_use]
    pub fn secs_since(self, earlier: Self) -> i64 {
        self.0.as_second().saturating_sub(earlier.0.as_second())
    }

    /// `self` moved `secs` seconds later (negative: earlier); saturates at the range ends.
    #[must_use]
    pub fn plus_secs(self, secs: i64) -> Self {
        let s = self.0.as_second().saturating_add(secs);
        jiff::Timestamp::from_second(s).map_or(self, Self)
    }

    /// The instant of a UTC calendar date and time. `None` for an impossible date or time.
    #[must_use]
    pub fn from_utc(
        year: i16,
        month: i8,
        day: i8,
        hour: i8,
        minute: i8,
        second: i8,
    ) -> Option<Self> {
        let dt = jiff::civil::DateTime::new(year, month, day, hour, minute, second, 0).ok()?;
        jiff::tz::Offset::UTC.to_timestamp(dt).ok().map(Self)
    }

    /// Microseconds since the Unix epoch (floor).
    #[must_use]
    pub fn unix_micros(self) -> i64 {
        self.0.as_microsecond()
    }

    /// The instant `micros` microseconds after the Unix epoch (negative: before it). `None` when out of range.
    #[must_use]
    pub fn from_unix_micros(micros: i64) -> Option<Self> {
        jiff::Timestamp::from_microsecond(micros).ok().map(Self)
    }

    /// Milliseconds since the Unix epoch (floor, as chrono's `timestamp_millis`).
    #[must_use]
    pub fn unix_millis(self) -> i64 {
        self.0.as_millisecond()
    }

    /// `strftime`-style text of this instant in UTC, e.g. `%Y-%m-%dT%H:%MZ`.
    #[must_use]
    pub fn format(self, fmt: &str) -> String {
        self.0.strftime(fmt).to_string()
    }

    /// RFC 3339 in UTC, e.g. `2026-10-30T23:00:00Z`.
    #[must_use]
    pub fn to_rfc3339(self) -> String {
        self.0.to_string()
    }
}

impl From<std::time::SystemTime> for Timestamp {
    fn from(t: std::time::SystemTime) -> Self {
        Self(jiff::Timestamp::try_from(t).unwrap_or(jiff::Timestamp::UNIX_EPOCH))
    }
}

impl From<Timestamp> for std::time::SystemTime {
    fn from(t: Timestamp) -> Self {
        Self::try_from(t.0).unwrap_or(Self::UNIX_EPOCH)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl FromStr for Timestamp {
    type Err = jiff::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(Self)
    }
}

/// A calendar day with no time of day, serialised `YYYY-MM-DD` (what `chrono::NaiveDate` wrote).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Date(jiff::civil::Date);

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl FromStr for Date {
    type Err = jiff::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_and_millis_match_what_chrono_printed() {
        let t: Timestamp = "2026-10-30T23:04:05.678Z".parse().unwrap();
        assert_eq!(t.format("%Y-%m-%dT%H:%MZ"), "2026-10-30T23:04Z");
        assert_eq!(t.format("%Y-%m-%dT%H:%M:%SZ"), "2026-10-30T23:04:05Z");
        assert_eq!(t.format("%Y%m%dT%H%M%SZ"), "20261030T230405Z");
        assert_eq!(t.format("%Y-%m-%d"), "2026-10-30");
        assert_eq!(t.unix_millis(), 1_793_401_445_678);
        let neg = Timestamp::from_unix(-1, 500_000_000).unwrap();
        assert_eq!(neg.unix_millis(), -500);
    }

    #[test]
    fn from_utc_builds_a_calendar_instant_and_refuses_an_impossible_one() {
        let t = Timestamp::from_utc(2026, 10, 30, 23, 0, 0).unwrap();
        assert_eq!(t.unix_secs(), 1_793_401_200);
        assert_eq!(t.to_rfc3339(), "2026-10-30T23:00:00Z");
        assert!(Timestamp::from_utc(2026, 2, 30, 0, 0, 0).is_none());
        assert!(Timestamp::from_utc(2026, 10, 30, 24, 0, 0).is_none());
    }

    #[test]
    fn micros_and_system_time_round_trip() {
        let t = Timestamp::from_unix_micros(1_790_000_000_123_456).unwrap();
        assert_eq!(t.unix_micros(), 1_790_000_000_123_456);
        assert_eq!(t.subsec_nanos(), 123_456_000);
        let back = Timestamp::from(std::time::SystemTime::from(t));
        assert_eq!(back, t);
        assert_eq!(Timestamp::from_unix_micros(-1).unwrap().unix_micros(), -1);
    }

    #[test]
    fn a_date_reads_and_writes_yyyy_mm_dd_and_refuses_an_impossible_day() {
        let d: Date = serde_json::from_str("\"2026-09-18\"").unwrap();
        assert_eq!(d.to_string(), "2026-09-18");
        assert_eq!(serde_json::to_string(&d).unwrap(), "\"2026-09-18\"");
        assert!(serde_json::from_str::<Date>("\"2026-02-30\"").is_err());
        assert!(serde_json::from_str::<Date>("\"2026-09-18T00:00:00Z\"").is_err());
        assert!(serde_json::from_str::<Date>("\"yesterday\"").is_err());
    }

    #[test]
    fn reads_what_chrono_wrote() {
        // chrono's serde output: nanosecond digits, `Z`.
        let t: Timestamp = serde_json::from_str("\"2026-10-30T23:00:00.123456789Z\"").unwrap();
        assert_eq!(t.unix_secs(), 1_793_401_200);
        // and an offset form, which chrono also accepted.
        let u: Timestamp = serde_json::from_str("\"2026-10-31T01:00:00+02:00\"").unwrap();
        assert_eq!(u.unix_secs(), 1_793_401_200);
    }

    #[test]
    fn writes_rfc3339_z() {
        let t = Timestamp::from_unix_secs(1_793_401_200).unwrap();
        assert_eq!(
            serde_json::to_string(&t).unwrap(),
            "\"2026-10-30T23:00:00Z\""
        );
        assert_eq!(t.to_rfc3339(), "2026-10-30T23:00:00Z");
    }

    #[test]
    fn arithmetic_and_order() {
        let a = Timestamp::from_unix_secs(1_000).unwrap();
        let b = a.plus_secs(90);
        assert_eq!(b.secs_since(a), 90);
        assert_eq!(a.secs_since(b), -90);
        assert!(b > a);
        assert_eq!(a.plus_secs(-1_000).unix_secs(), 0);
    }

    #[test]
    fn rejects_garbage() {
        assert!(serde_json::from_str::<Timestamp>("\"not a time\"").is_err());
        assert!(serde_json::from_str::<Timestamp>("12").is_err());
    }
}
