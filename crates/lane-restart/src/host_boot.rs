// SPDX-License-Identifier: MIT OR Apache-2.0

//! The boot record of a lane host: the first thing `lane-restart host` does, before it parses its
//! arguments, loads approvals or starts claude.
//!
//! 2026-10-18: three restarts failed with no new host log and no host process, so nobody could say
//! whether `wt.exe` never opened a window, the host died on the way up, or claude never started.
//! A host's own log is written only after its approvals load, so it cannot tell those apart. This
//! file can: if a restart finds no boot record for its role after the spawn, the host never ran.

use std::path::Path;
use std::time::{Duration, SystemTime};

const PREFIX: &str = "host-boot-";
const SUFFIX: &str = ".txt";

/// Boot records older than this are removed by the next host to start.
const KEEP: Duration = Duration::from_secs(2 * 24 * 3600);

/// Best effort, and never fatal: a host that cannot write its record still runs the lane.
/// `argv` is the host's own arguments after the program name (`host --role r -- claude ...`).
pub fn record(state_dir: &Path, argv: &[String]) {
    let _ = std::fs::create_dir_all(state_dir);
    let mut body = format!("{}\n", chrono::Utc::now().to_rfc3339());
    for a in argv {
        body.push_str(a);
        body.push('\n');
    }
    let _ = std::fs::write(
        state_dir.join(format!("{PREFIX}{}{SUFFIX}", std::process::id())),
        body,
    );
    sweep(state_dir, SystemTime::now());
}

fn is_record_name(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .is_some_and(|n| n.starts_with(PREFIX) && n.ends_with(SUFFIX))
}

fn sweep(state_dir: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(state_dir) else {
        return;
    };
    for e in entries.flatten() {
        if !is_record_name(&e.file_name()) {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| now.duration_since(t).is_ok_and(|age| age > KEEP));
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Whether a host for `role` wrote its boot record at or after `since`.
pub fn started_since(state_dir: &Path, role: &str, since: SystemTime) -> bool {
    let Ok(entries) = std::fs::read_dir(state_dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        is_record_name(&e.file_name())
            && e.metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| t >= since)
            && std::fs::read_to_string(e.path()).is_ok_and(|body| names_role(&body, role))
    })
}

/// The record lists the date, then one argument per line: the role is the line after `--role`.
fn names_role(body: &str, role: &str) -> bool {
    let mut lines = body.lines().skip(1);
    while let Some(l) = lines.next() {
        if l == "--role" {
            return lines.next() == Some(role);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(role: &str) -> Vec<String> {
        ["host", "--role", role, "--", "claude", "p"]
            .map(String::from)
            .to_vec()
    }

    #[test]
    fn a_record_is_found_for_its_own_role_only_and_only_since_the_spawn() {
        let d = tempfile::tempdir().unwrap();
        let before = SystemTime::now() - Duration::from_secs(5);
        record(d.path(), &argv("fuel"));
        assert!(started_since(d.path(), "fuel", before));
        assert!(
            !started_since(d.path(), "fuel-2", before),
            "a prefix is not the role"
        );
        assert!(!started_since(d.path(), "agentlife", before));
        assert!(
            !started_since(
                d.path(),
                "fuel",
                SystemTime::now() + Duration::from_secs(60)
            ),
            "a record older than the spawn does not count"
        );
    }

    #[test]
    fn a_missing_directory_is_no_record_not_a_panic() {
        let d = tempfile::tempdir().unwrap();
        assert!(!started_since(
            &d.path().join("nope"),
            "fuel",
            SystemTime::UNIX_EPOCH
        ));
    }

    #[test]
    fn old_records_are_swept_and_other_files_are_left_alone() {
        let d = tempfile::tempdir().unwrap();
        let old = d.path().join("host-boot-1.txt");
        let keep = d.path().join("host-fuel-1.log");
        std::fs::write(&old, "x").unwrap();
        std::fs::write(&keep, "x").unwrap();
        sweep(d.path(), SystemTime::now() + KEEP + Duration::from_secs(60));
        assert!(!old.exists());
        assert!(keep.exists());
    }
}
