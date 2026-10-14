// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart specs retire` - take a role whose directory is gone out of the live lane state,
//! without deleting anything. `specs import` promises never to edit a state file, so removing a
//! dead role from `status` is a separate, PM-only act, and this is it.
//!
//! A role is retired only when it cannot be resumed and nothing is running as it: its recorded
//! `cwd` is not a directory, and its recorded process is not live. Its `<role>.json` (and its
//! `<role>.launch.json`, if any) are **moved** to `<state dir>/retired/<file>.<UTC stamp>`, never
//! deleted, and the move is read back. A role that fails any check is listed with the reason.

use crate::facts::SystemFacts;
use crate::relaunch::valid_identifier;
use crate::spec;
use crate::state;
use chrono::{DateTime, Utc};
use std::path::Path;

/// The role the PM session runs as: never retired.
const PM_ROLE: &str = "pm";

#[derive(Debug, PartialEq, Eq)]
pub enum RetireOutcome {
    /// Moved (file names, as they were).
    Moved(Vec<String>),
    /// `--dry-run`: these files would be moved.
    WouldMove(Vec<String>),
    /// Not retired; the reason is the whole answer.
    Refused(String),
}

#[derive(Debug, PartialEq, Eq)]
pub struct RetireRow {
    pub role: String,
    pub outcome: RetireOutcome,
}

fn retire_one(
    dir: &Path,
    role: &str,
    facts: &dyn SystemFacts,
    dry_run: bool,
    now: DateTime<Utc>,
) -> RetireOutcome {
    if !valid_identifier(role) {
        return RetireOutcome::Refused(format!("not a plain role identifier: {role:?}"));
    }
    if role == PM_ROLE {
        return RetireOutcome::Refused("role pm is never retired".to_string());
    }
    let st = match state::load(dir, role) {
        Ok(s) => s,
        Err(e) => return RetireOutcome::Refused(format!("no readable lane state: {e}")),
    };
    if Path::new(&st.cwd).is_dir() {
        return RetireOutcome::Refused(format!(
            "cwd exists: {} (a role with a directory is resumed with `start`, not retired)",
            st.cwd
        ));
    }
    if spec::state_process_is_live(&st, facts) {
        return RetireOutcome::Refused(format!(
            "its recorded process (pid {}) is live: stop it first",
            st.pid
        ));
    }
    let mut files = vec![format!("{role}.json")];
    let launch = format!("{role}{}", spec::FILE_SUFFIX);
    if dir.join(&launch).exists() {
        files.push(launch);
    }
    if dry_run {
        return RetireOutcome::WouldMove(files);
    }
    let dest = dir.join("retired");
    if let Err(e) = std::fs::create_dir_all(&dest) {
        return RetireOutcome::Refused(format!("cannot create {}: {e}", dest.display()));
    }
    let stamp = now.format("%Y%m%dT%H%M%SZ");
    for (i, f) in files.iter().enumerate() {
        let (from, to) = (dir.join(f), dest.join(format!("{f}.{stamp}")));
        if to.exists() {
            return RetireOutcome::Refused(format!(
                "{} already exists; nothing moved past {} file(s)",
                to.display(),
                i
            ));
        }
        if let Err(e) = std::fs::rename(&from, &to) {
            return RetireOutcome::Refused(format!(
                "could not move {f}: {e} ({i} file(s) already moved)"
            ));
        }
        // Read it back: the file is at the destination and no longer at the source.
        if !to.is_file() || from.exists() {
            return RetireOutcome::Refused(format!("{f}: move did not read back"));
        }
    }
    RetireOutcome::Moved(files)
}

/// One row per requested role, in the order given.
pub fn retire_roles(
    dir: &Path,
    roles: &[String],
    facts: &dyn SystemFacts,
    dry_run: bool,
    now: DateTime<Utc>,
) -> Vec<RetireRow> {
    roles
        .iter()
        .map(|role| RetireRow {
            role: role.clone(),
            outcome: retire_one(dir, role, facts, dry_run, now),
        })
        .collect()
}

/// The report: first and last line say whether anything moved.
pub fn render(rows: &[RetireRow], dry_run: bool) -> String {
    let mut out = String::from(if dry_run {
        "DRY RUN - nothing moved\n"
    } else {
        "RETIRE - moving lane state to retired/ (a dry run is --dry-run)\n"
    });
    let (mut moved, mut would) = (0usize, 0usize);
    for r in rows {
        match &r.outcome {
            RetireOutcome::Moved(f) => {
                moved += 1;
                out.push_str(&format!("{}\tretired\t{}\n", r.role, f.join(", ")));
            }
            RetireOutcome::WouldMove(f) => {
                would += 1;
                out.push_str(&format!("{}\twould retire\t{}\n", r.role, f.join(", ")));
            }
            RetireOutcome::Refused(why) => {
                out.push_str(&format!("{}\trefused\t{why}\n", r.role));
            }
        }
    }
    out.push_str(&format!("({} role(s) considered)\n", rows.len()));
    if dry_run {
        out.push_str(&format!(
            "DRY RUN - nothing moved ({would} would be retired)\n"
        ));
    } else {
        out.push_str(&format!("RETIRED {moved} role(s)\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{log, state_at, RecordingFacts};
    use tempfile::tempdir;

    /// A state file for `role` whose cwd is `cwd`, recorded as pid `pid`.
    fn state_file(dir: &Path, role: &str, cwd: &Path, pid: u32) {
        let mut st = state_at(role, "s", "PreToolUse");
        st.pid = pid;
        st.pid_start_secs = Some(100);
        st.cwd = cwd.to_string_lossy().into_owned();
        std::fs::write(
            dir.join(format!("{role}.json")),
            serde_json::to_string(&st).unwrap(),
        )
        .unwrap();
    }

    fn run(d: &Path, roles: &[&str], dry: bool) -> Vec<RetireRow> {
        let facts = RecordingFacts::new(&log());
        let roles: Vec<String> = roles.iter().map(|s| s.to_string()).collect();
        retire_roles(d, &roles, &facts, dry, Utc::now())
    }

    fn gone() -> std::path::PathBuf {
        let t = tempdir().unwrap();
        let p = t.path().join("worktree");
        // `t` drops here: the parent is removed, so `p` does not exist.
        drop(t);
        p
    }

    #[test]
    fn a_role_with_a_missing_cwd_is_moved_not_deleted_and_read_back() {
        let d = tempdir().unwrap();
        state_file(d.path(), "wt1", &gone(), 1);
        std::fs::write(d.path().join("wt1.launch.json"), "{}").unwrap();
        let rows = run(d.path(), &["wt1"], false);
        assert_eq!(
            rows[0].outcome,
            RetireOutcome::Moved(vec!["wt1.json".into(), "wt1.launch.json".into()])
        );
        assert!(!d.path().join("wt1.json").exists());
        assert!(!d.path().join("wt1.launch.json").exists());
        let kept: Vec<String> = std::fs::read_dir(d.path().join("retired"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(kept.len(), 2, "{kept:?}");
        assert!(kept.iter().any(|n| n.starts_with("wt1.json.")));
        assert!(kept.iter().any(|n| n.starts_with("wt1.launch.json.")));
    }

    #[test]
    fn a_dry_run_moves_nothing() {
        let d = tempdir().unwrap();
        state_file(d.path(), "wt1", &gone(), 1);
        let rows = run(d.path(), &["wt1"], true);
        assert_eq!(
            rows[0].outcome,
            RetireOutcome::WouldMove(vec!["wt1.json".into()])
        );
        assert!(d.path().join("wt1.json").exists());
        assert!(!d.path().join("retired").exists());
    }

    #[test]
    fn a_role_with_a_directory_is_refused_and_untouched() {
        let d = tempdir().unwrap();
        let cwd = tempdir().unwrap();
        state_file(d.path(), "alive-dir", cwd.path(), 1);
        let rows = run(d.path(), &["alive-dir"], false);
        assert!(
            matches!(&rows[0].outcome, RetireOutcome::Refused(w) if w.starts_with("cwd exists"))
        );
        assert!(d.path().join("alive-dir.json").exists());
    }

    #[test]
    fn a_live_process_pm_a_bad_name_and_an_unknown_role_are_refused() {
        let d = tempdir().unwrap();
        state_file(d.path(), "running", &gone(), 55);
        state_file(d.path(), "pm", &gone(), 1);
        let mut facts = RecordingFacts::new(&log());
        facts.live_pid = Some((55, 100));
        let roles: Vec<String> = ["running", "pm", "../x", "nosuch"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let rows = retire_roles(d.path(), &roles, &facts, false, Utc::now());
        let why = |i: usize| match &rows[i].outcome {
            RetireOutcome::Refused(w) => w.clone(),
            o => panic!("row {i} not refused: {o:?}"),
        };
        assert!(why(0).contains("is live"), "{}", why(0));
        assert!(why(1).contains("never retired"), "{}", why(1));
        assert!(why(2).contains("plain role identifier"), "{}", why(2));
        assert!(why(3).contains("no readable lane state"), "{}", why(3));
        assert!(d.path().join("running.json").exists());
        assert!(d.path().join("pm.json").exists());
    }

    #[test]
    fn an_existing_destination_is_never_overwritten() {
        let d = tempdir().unwrap();
        state_file(d.path(), "wt1", &gone(), 1);
        let now = Utc::now();
        let stamp = now.format("%Y%m%dT%H%M%SZ");
        std::fs::create_dir(d.path().join("retired")).unwrap();
        let taken = d.path().join("retired").join(format!("wt1.json.{stamp}"));
        std::fs::write(&taken, "evidence").unwrap();
        let facts = RecordingFacts::new(&log());
        let rows = retire_roles(d.path(), &["wt1".to_string()], &facts, false, now);
        assert!(
            matches!(&rows[0].outcome, RetireOutcome::Refused(w) if w.contains("already exists"))
        );
        assert_eq!(std::fs::read_to_string(&taken).unwrap(), "evidence");
        assert!(d.path().join("wt1.json").exists());
    }

    #[test]
    fn the_report_says_first_and_last_whether_anything_moved() {
        let d = tempdir().unwrap();
        state_file(d.path(), "wt1", &gone(), 1);
        let dry = render(&run(d.path(), &["wt1"], true), true);
        assert_eq!(dry.lines().next(), Some("DRY RUN - nothing moved"));
        assert_eq!(
            dry.lines().last(),
            Some("DRY RUN - nothing moved (1 would be retired)")
        );
        let real = render(&run(d.path(), &["wt1"], false), false);
        assert_eq!(real.lines().last(), Some("RETIRED 1 role(s)"));
        assert!(real.contains("wt1\tretired\twt1.json"), "{real}");
    }
}
