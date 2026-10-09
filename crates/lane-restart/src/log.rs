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
        }
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
