// SPDX-License-Identifier: MIT OR Apache-2.0

//! `C:/Projects/.lane-state/restart.log` — RESTART-TOOL-DESIGN.md §6.3.
//!
//! ⚠️ APPEND-ONLY, NEVER OVERWRITTEN, NEVER ROTATED SILENTLY. One line per
//! event, JSON, so it's both machine-readable and diffable. Every entry -
//! refused or acted - is written, not just the successful ones: a log that
//! only records what happened when the tool acted would hide every refusal,
//! which is exactly the evidence this portfolio's own culture depends on.

use chrono::{DateTime, Utc};
use serde::Serialize;
use std::io::Write;
use std::path::Path;

#[derive(Debug, Serialize)]
pub struct Entry<'a> {
    pub timestamp: DateTime<Utc>,
    /// "self" or "pm"
    pub requested_by: &'a str,
    pub role: &'a str,
    /// The full outcome, human-readable: either what was checked and
    /// refused (with why), or what was done (killed pid X, launched
    /// `claude --resume ...`).
    pub outcome: String,
    pub acted: bool,
    /// RESTART-TOOL-DESIGN.md 14.7: added additively, so lines written before them (and readers
    /// that ignore unknown keys) are unaffected. `stop`, `start`, `restart` or `import`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<&'a str>,
    /// The verified role of the caller (14.8), when it could be established.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_role: Option<&'a str>,
    /// The model a start passed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<&'a str>,
    /// Why a lane was started (required by `lane-start`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'a str>,
    /// The Synapse signal a launcher start answered (0.15.13).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal_id: Option<&'a str>,
    /// A start's `--json` result as `<outcome>:<reason>` (0.15.13).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<&'a str>,
    /// Whether a start tried to spawn; the launcher caps count rows where this is true (0.15.13).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spawned: Option<bool>,
}

#[derive(Debug)]
pub struct AppendError(pub String);

impl std::fmt::Display for AppendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "could not append to the restart log: {}", self.0)
    }
}

/// Appends one JSON line. Never truncates, never opens for write-replace -
/// `OpenOptions::append` is the whole safety property here.
pub fn append(log_path: &Path, entry: &Entry) -> Result<(), AppendError> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AppendError(e.to_string()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|e| AppendError(e.to_string()))?;
    let line = serde_json::to_string(entry).map_err(|e| AppendError(e.to_string()))?;
    writeln!(file, "{line}").map_err(|e| AppendError(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample<'a>(role: &'a str, outcome: &str, acted: bool) -> Entry<'a> {
        Entry {
            timestamp: Utc::now(),
            requested_by: "self",
            role,
            outcome: outcome.to_string(),
            acted,
            kind: None,
            caller_role: None,
            model: None,
            reason: None,
            signal_id: None,
            result: None,
            spawned: None,
        }
    }

    /// Lines written before the 14.7 fields must not grow them: a reader that treats an unknown
    /// key as an error would otherwise break on every old-style entry.
    #[test]
    fn an_entry_without_the_new_fields_serialises_exactly_as_before() {
        let line = serde_json::to_value(sample("overmind", "x", false)).unwrap();
        let keys: Vec<&str> = line
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        for k in [
            "kind",
            "caller_role",
            "model",
            "reason",
            "signal_id",
            "result",
            "spawned",
        ] {
            assert!(!keys.contains(&k), "{k} serialised when None: {keys:?}");
        }
    }

    #[test]
    fn the_start_fields_are_written_when_set() {
        let mut e = sample("baracuda", "started", true);
        e.kind = Some("start");
        e.caller_role = Some("pm");
        e.model = Some("sonnet");
        e.reason = Some("resume after crash");
        let v = serde_json::to_value(e).unwrap();
        assert_eq!(v["kind"], "start");
        assert_eq!(v["caller_role"], "pm");
        assert_eq!(v["model"], "sonnet");
        assert_eq!(v["reason"], "resume after crash");
    }

    #[test]
    fn appends_a_line_creating_the_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("restart.log");
        append(&path, &sample("overmind", "killed and relaunched", true)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("overmind"));
        assert!(text.trim_end().lines().count() == 1);
    }

    #[test]
    fn a_second_append_does_not_overwrite_the_first() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("restart.log");
        append(&path, &sample("overmind", "refused: busy", false)).unwrap();
        append(&path, &sample("synapse", "killed and relaunched", true)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.trim_end().lines().collect();
        assert_eq!(lines.len(), 2, "both entries must survive, in order");
        assert!(lines[0].contains("overmind"));
        assert!(lines[1].contains("synapse"));
    }

    #[test]
    fn creates_the_parent_directory_if_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("restart.log");
        append(&path, &sample("overmind", "x", false)).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn each_line_is_valid_json() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("restart.log");
        append(&path, &sample("overmind", "killed and relaunched", true)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(text.trim_end()).unwrap();
        assert_eq!(parsed["role"], "overmind");
        assert_eq!(parsed["acted"], true);
    }
}
