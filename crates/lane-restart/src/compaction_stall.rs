// SPDX-License-Identifier: MIT OR Apache-2.0

//! Detects a lane that resumed after a context compaction and then wrote nothing (PM finding
//! 2026-11-23: three lanes silent 33-51 min after a "This session is being continued" entry while
//! `claude.exe` stayed alive). ROADMAP O21.
//!
//! Evidence, READ on 2026-11-24 over 889 real entries in `~/.claude/projects/C--Projects-*/*.jsonl`: a
//! real continuation is a line with `type:"user"`, `isCompactSummary:true` and a `timestamp`. The same
//! prose also appears inside quoted peer messages, so the text alone is not the signal.
//!
//! Pure apart from the two readers. A lane is stalled when, after its LAST compact-summary entry, no
//! `assistant` entry follows. The number is the age of that summary; the caller applies the threshold.

use chrono::{DateTime, Utc};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Age of an unanswered compact summary at which `status` calls it a stall.
pub const STALL_AFTER_SECS: i64 = 600;
/// How much of a transcript's end is read (a compact summary line can be tens of KB).
pub const TAIL_BYTES: u64 = 2 * 1024 * 1024;

/// Claude's per-project directory name for a working directory: every character that is not
/// ASCII alphanumeric becomes `-` (`C:\Projects\agentlife` is `C--Projects-agentlife`).
pub fn project_dir_name(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Seconds since the last compact-summary entry in `jsonl`, when no `assistant` entry follows it;
/// `None` when there is no such entry or the lane has answered since. Lines that are not JSON (a
/// truncated first line of a tail) are skipped.
pub fn stalled_secs(jsonl: &str, now: DateTime<Utc>) -> Option<i64> {
    let mut pending: Option<DateTime<Utc>> = None;
    for line in jsonl.lines() {
        // Cheap pre-filters: most lines are neither, and a summary line is large.
        let is_summary = line.contains("\"isCompactSummary\":true");
        let is_assistant = line.contains("\"type\":\"assistant\"");
        if !is_summary && !is_assistant {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let ty = v.get("type").and_then(|t| t.as_str());
        if ty == Some("user") && v.get("isCompactSummary").and_then(|b| b.as_bool()) == Some(true) {
            pending = v
                .get("timestamp")
                .and_then(|t| t.as_str())
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                .map(|t| t.with_timezone(&Utc));
        } else if ty == Some("assistant") {
            pending = None;
        }
    }
    pending.map(|t| (now - t).num_seconds().max(0))
}

/// The newest `*.jsonl` directly in `dir` (a lane's transcripts: the live session writes the newest).
pub fn newest_transcript(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p)
}

/// The last [`TAIL_BYTES`] of `path` as text, without a partial first line.
pub fn read_tail(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    if start > 0 {
        return Ok(text
            .split_once('\n')
            .map(|x| x.1.to_string())
            .unwrap_or_default());
    }
    Ok(text)
}

/// [`stalled_secs`] for the lane whose working directory is `cwd`, under `projects_root`
/// (`~/.claude/projects`). `None` when it cannot be read: never a guess.
pub fn check(projects_root: &Path, cwd: &str, now: DateTime<Utc>) -> Option<i64> {
    let file = newest_transcript(&projects_root.join(project_dir_name(cwd)))?;
    stalled_secs(&read_tail(&file).ok()?, now)
}

/// Fills `compaction_stall_secs` for every live role from its newest transcript under `projects_root`.
pub fn annotate(
    report: &mut crate::spec::StatusReport,
    state_dir: &Path,
    projects_root: &Path,
    now: DateTime<Utc>,
) {
    for r in &mut report.roles {
        let Some(a) = &mut r.activity else { continue };
        if let Ok(st) = crate::state::load(state_dir, &r.role) {
            a.compaction_stall_secs = check(projects_root, &st.cwd, now);
        }
    }
}

/// ` COMPACT-STALL=Ns` once a lane has been silent for [`STALL_AFTER_SECS`] after a compaction.
pub fn mark(secs: Option<i64>) -> String {
    match secs {
        Some(s) if s >= STALL_AFTER_SECS => format!(" COMPACT-STALL={s}s"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_mark_appears_only_at_the_threshold() {
        assert_eq!(super::mark(None), "");
        assert_eq!(super::mark(Some(599)), "");
        assert_eq!(super::mark(Some(600)), " COMPACT-STALL=600s");
    }

    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }
    const NOW: &str = "2026-11-24T10:00:00Z";

    fn summary(ts: &str) -> String {
        format!(
            r#"{{"type":"user","isCompactSummary":true,"isVisibleInTranscriptOnly":true,"timestamp":"{ts}","message":{{"role":"user","content":"This session is being continued from a previous conversation"}}}}"#
        )
    }
    fn assistant(ts: &str) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{ts}","message":{{"role":"assistant","content":[]}}}}"#
        )
    }
    fn queue(ts: &str) -> String {
        format!(r#"{{"type":"queue-operation","operation":"enqueue","timestamp":"{ts}"}}"#)
    }

    #[test]
    fn project_dir_name_matches_claudes_encoding() {
        assert_eq!(
            project_dir_name(r"C:\Projects\agentlife"),
            "C--Projects-agentlife"
        );
        assert_eq!(
            project_dir_name(r"C:\Projects\agentlife-wt-stall"),
            "C--Projects-agentlife-wt-stall"
        );
    }

    #[test]
    fn an_unanswered_summary_is_a_stall_with_its_age() {
        let j = format!(
            "{}\n{}\n{}\n",
            assistant("2026-11-24T08:00:00Z"),
            summary("2026-11-24T09:00:00Z"),
            queue("2026-11-24T09:30:00Z")
        );
        assert_eq!(stalled_secs(&j, t(NOW)), Some(3600));
    }

    #[test]
    fn an_assistant_entry_after_the_summary_clears_it() {
        let j = format!(
            "{}\n{}\n",
            summary("2026-11-24T09:00:00Z"),
            assistant("2026-11-24T09:01:00Z")
        );
        assert_eq!(stalled_secs(&j, t(NOW)), None);
    }

    #[test]
    fn only_the_last_summary_counts() {
        let j = format!(
            "{}\n{}\n{}\n",
            summary("2026-11-24T07:00:00Z"),
            assistant("2026-11-24T07:05:00Z"),
            summary("2026-11-24T09:50:00Z")
        );
        assert_eq!(stalled_secs(&j, t(NOW)), Some(600));
    }

    // The control that makes the first test mean something: the same prose QUOTED inside a peer message
    // (a different line type, no isCompactSummary) is not a continuation.
    #[test]
    fn quoted_continuation_text_is_not_a_summary() {
        let quoted = r#"{"type":"user","isMeta":true,"timestamp":"2026-11-24T09:00:00Z","message":{"role":"user","content":"a lane said: This session is being continued from a previous conversation"}}"#;
        assert_eq!(stalled_secs(&format!("{quoted}\n"), t(NOW)), None);
        assert_eq!(stalled_secs("", t(NOW)), None);
    }

    #[test]
    fn a_truncated_or_garbled_line_is_skipped_not_fatal() {
        let j = format!(
            "{{\"isCompactSummary\":true, broken\n{}\n",
            summary("2026-11-24T09:00:00Z")
        );
        assert_eq!(stalled_secs(&j, t(NOW)), Some(3600));
    }

    #[test]
    fn check_reads_the_newest_transcript_of_the_project_directory() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("C--Projects-lane");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("old.jsonl"),
            format!("{}\n", assistant("2026-11-24T01:00:00Z")),
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(
            dir.join("new.jsonl"),
            format!("{}\n", summary("2026-11-24T09:00:00Z")),
        )
        .unwrap();
        assert_eq!(check(root.path(), r"C:\Projects\lane", t(NOW)), Some(3600));
        assert_eq!(
            check(root.path(), r"C:\Projects\other", t(NOW)),
            None,
            "no such project directory"
        );
    }

    #[test]
    fn a_tail_read_drops_the_partial_first_line() {
        let root = tempfile::tempdir().unwrap();
        let p = root.path().join("x.jsonl");
        let filler = "x".repeat(TAIL_BYTES as usize);
        std::fs::write(
            &p,
            format!("{filler}\n{}\n", summary("2026-11-24T09:00:00Z")),
        )
        .unwrap();
        let tail = read_tail(&p).unwrap();
        assert!(
            tail.starts_with("{\"type\":\"user\""),
            "{}",
            &tail[..40.min(tail.len())]
        );
        assert_eq!(stalled_secs(&tail, t(NOW)), Some(3600));
    }
}
