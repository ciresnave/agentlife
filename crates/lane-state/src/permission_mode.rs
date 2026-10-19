// SPDX-License-Identifier: MIT OR Apache-2.0

//! The one definition of a Claude Code permission mode.
//!
//! A mode crosses three boundaries as a plain string: the hook payload claude writes, the
//! `--permission-mode` launch flag, and the files lane-restart and agentlife store. `claude` adds
//! modes over time, so a spelling this build does not know reads as `Other` rather than refusing
//! the file or payload, and it is never guessed to be a known one (`is_known`).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    Auto,
    BypassPermissions,
    Other(String),
}

impl PermissionMode {
    /// The modes agentlife knows, narrowest first (the order `rank` uses).
    const KNOWN: [PermissionMode; 4] = [
        PermissionMode::Default,
        PermissionMode::AcceptEdits,
        PermissionMode::Auto,
        PermissionMode::BypassPermissions,
    ];

    pub fn as_str(&self) -> &str {
        match self {
            PermissionMode::Default => "default",
            PermissionMode::AcceptEdits => "acceptEdits",
            PermissionMode::Auto => "auto",
            PermissionMode::BypassPermissions => "bypassPermissions",
            PermissionMode::Other(s) => s,
        }
    }

    pub fn is_known(&self) -> bool {
        !matches!(self, PermissionMode::Other(_))
    }

    /// Wider is higher; a mode this build does not know ranks above every known one, so it is
    /// always shown as widened.
    pub fn rank(&self) -> usize {
        Self::KNOWN
            .iter()
            .position(|k| k == self)
            .unwrap_or(Self::KNOWN.len())
    }
}

impl From<&str> for PermissionMode {
    fn from(s: &str) -> Self {
        match s {
            "default" => PermissionMode::Default,
            "acceptEdits" => PermissionMode::AcceptEdits,
            "auto" => PermissionMode::Auto,
            "bypassPermissions" => PermissionMode::BypassPermissions,
            other => PermissionMode::Other(other.to_string()),
        }
    }
}

impl From<String> for PermissionMode {
    fn from(s: String) -> Self {
        s.as_str().into()
    }
}

impl std::fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for PermissionMode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PermissionMode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(String::deserialize(d)?.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_spelling_is_its_variant_and_an_unknown_one_is_kept_verbatim() {
        for (s, m) in [
            ("default", PermissionMode::Default),
            ("acceptEdits", PermissionMode::AcceptEdits),
            ("auto", PermissionMode::Auto),
            ("bypassPermissions", PermissionMode::BypassPermissions),
        ] {
            assert_eq!(PermissionMode::from(s), m);
            assert_eq!(m.as_str(), s);
            assert!(m.is_known());
        }
        let odd = PermissionMode::from("plan");
        assert_eq!(odd, PermissionMode::Other("plan".into()));
        assert_eq!(odd.as_str(), "plan");
        assert!(!odd.is_known());
        // case is part of the spelling: a near miss is not the known mode
        assert!(!PermissionMode::from("Auto").is_known());
    }

    #[test]
    fn it_is_a_plain_string_on_disk_both_ways() {
        let m: PermissionMode = serde_json::from_str("\"acceptEdits\"").unwrap();
        assert_eq!(m, PermissionMode::AcceptEdits);
        assert_eq!(serde_json::to_string(&m).unwrap(), "\"acceptEdits\"");
        let o: PermissionMode = serde_json::from_str("\"newmode\"").unwrap();
        assert_eq!(serde_json::to_string(&o).unwrap(), "\"newmode\"");
    }

    #[test]
    fn rank_widens_and_an_unknown_mode_outranks_every_known_one() {
        let r = |s: &str| PermissionMode::from(s).rank();
        assert!(r("default") < r("acceptEdits"));
        assert!(r("acceptEdits") < r("auto"));
        assert!(r("auto") < r("bypassPermissions"));
        assert!(r("bypassPermissions") < r("something-new"));
    }
}
