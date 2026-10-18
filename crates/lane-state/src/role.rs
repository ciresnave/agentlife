// SPDX-License-Identifier: MIT OR Apache-2.0

//! The one definition of a lane role (and of every name that follows the same rule: an agent id,
//! a launch name). A role becomes a file name, an env value, a `wt.exe`/`conhost.exe` argument and
//! part of a Synapse signal id, so it is a plain identifier or it is nothing: `[A-Za-z0-9_-]{1,64}`.
//! Eleven hand-copied checks of this rule used to live in four crates; any one drifting would have
//! let a different set of strings through one door than another.

use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;

/// Longest role, in bytes (the characters are all ASCII).
pub const MAX_ROLE_LEN: usize = 64;

/// Whether `s` is a plain identifier: `[A-Za-z0-9_-]{1,64}`. Prefer [`Role`] where a value is kept;
/// use this where only a yes/no is needed.
pub fn is_valid(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_ROLE_LEN
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A validated role. The only ways to make one are [`Role::new`] and `Deserialize` (which calls it),
/// so holding a `Role` is proof the string is a plain identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Role(String);

/// The string that was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidRole(pub String);

impl fmt::Display for InvalidRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} is not a plain identifier ([A-Za-z0-9_-], 1-{MAX_ROLE_LEN} characters)",
            self.0
        )
    }
}

impl std::error::Error for InvalidRole {}

impl Role {
    pub fn new(s: impl Into<String>) -> Result<Self, InvalidRole> {
        let s = s.into();
        if is_valid(&s) {
            Ok(Self(s))
        } else {
            Err(InvalidRole(s))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for Role {
    type Err = InvalidRole;

    fn from_str(s: &str) -> Result<Self, InvalidRole> {
        Self::new(s)
    }
}

impl TryFrom<String> for Role {
    type Error = InvalidRole;

    fn try_from(s: String) -> Result<Self, InvalidRole> {
        Self::new(s)
    }
}

impl AsRef<str> for Role {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Role {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::new(s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_identifiers_and_the_length_edges() {
        for ok in ["overmind", "pm-2", "lane_42", "A", &"x".repeat(64)] {
            assert!(is_valid(ok), "{ok}");
            assert_eq!(Role::new(ok).unwrap().as_str(), ok);
        }
    }

    #[test]
    fn refuses_everything_a_shell_or_wt_or_a_path_would_read() {
        for bad in [
            "",
            "a b",
            "a;b",
            "a&b",
            "a|b",
            "a^b",
            "a%b",
            "a<b",
            "a>b",
            "a.b",
            "a/b",
            r"a\b",
            "../x",
            "a:b",
            "caf\u{e9}",
            "a\nb",
            &"x".repeat(65),
        ] {
            assert!(!is_valid(bad), "{bad:?}");
            assert_eq!(Role::new(bad).unwrap_err(), InvalidRole(bad.to_string()));
        }
    }

    #[test]
    fn deserializing_checks_and_serializing_is_transparent() {
        let r: Role = serde_json::from_str("\"pm-2\"").unwrap();
        assert_eq!(serde_json::to_string(&r).unwrap(), "\"pm-2\"");
        let err = serde_json::from_str::<Role>("\"a;b\"")
            .unwrap_err()
            .to_string();
        assert!(err.contains("not a plain identifier"), "{err}");
        assert!("x y".parse::<Role>().is_err());
        assert_eq!("x-y".parse::<Role>().unwrap().to_string(), "x-y");
    }
}
