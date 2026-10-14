// SPDX-License-Identifier: MIT OR Apache-2.0
//! A lane's launch spec (`<role>.launch.json`, written by `lane-restart`) as the planner's
//! preferred way back (the PM's ruling of 2026-10-14, option A): **spec first, the registry as the
//! flagged fallback**. One launch description for `lane-restart start` and for `restore`, so a lane
//! that is resumable by one path is resumable by the other with the same allowlist and model pin.
//!
//! Only a **complete** spec is used (`SpecFile::check`: schema known, explicit non-Opus model, a
//! plain role and name, no `;`). An incomplete, unreadable or missing one is `None`, and the planner
//! then falls back to the registry record and says so on the entry.
//!
//! The spec is turned into a recorded-style argv and handed to the same `plan::rebuild` as a
//! registry record's, so nothing here widens what is launched: the planner's allowlist, its model
//! pin and its bypass-is-PM-only rule all still apply to it.

use lane_restart::spec;
use std::path::Path;

/// What the planner needs from a spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecLaunch {
    /// The directory the lane is started in.
    pub cwd: String,
    /// A recorded-style argument list (no program name).
    pub argv: Vec<String>,
}

/// Turns a spec into a [`SpecLaunch`]. Pure.
pub fn from_spec(s: &spec::SpecFile) -> SpecLaunch {
    let mut argv = vec![
        "--name".to_string(),
        s.name.clone(),
        "--model".to_string(),
        s.model.clone(),
    ];
    if let Some(m) = &s.permission_mode {
        argv.push("--permission-mode".to_string());
        argv.push(m.clone());
    }
    if s.remote_control {
        argv.push("--remote-control".to_string());
    }
    argv.extend(s.launch_args.iter().cloned());
    SpecLaunch {
        cwd: s.cwd.clone(),
        argv,
    }
}

/// The role's complete spec in `lane_state_dir`, or `None` when there is none, it cannot be read,
/// or it is not complete.
pub fn load(lane_state_dir: &Path, role: &str) -> Option<SpecLaunch> {
    let s = spec::read(lane_state_dir, role).ok()?;
    s.check().ok()?;
    Some(from_spec(&s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn spec_of(role: &str, model: &str) -> spec::SpecFile {
        spec::SpecFile {
            schema: spec::SCHEMA,
            role: role.into(),
            name: role.into(),
            cwd: format!("C:/Projects/{role}"),
            model: model.into(),
            permission_mode: Some("auto".into()),
            remote_control: true,
            launch_args: vec![
                "--dangerously-load-development-channels".into(),
                "server:claude-peers".into(),
            ],
            handoff_path: None,
            handoff_mtime: None,
            state: spec::SpecState::Suspended,
            written_at: Utc::now(),
            written_by: "test".into(),
            stopped_session_id: None,
            stopped_pid_start_secs: None,
            stop_reason: None,
            tool_version: "test".into(),
        }
    }

    #[test]
    fn a_complete_spec_becomes_one_argv_in_a_fixed_order() {
        let l = from_spec(&spec_of("alpha", "sonnet"));
        assert_eq!(l.cwd, "C:/Projects/alpha");
        assert_eq!(
            l.argv,
            [
                "--name",
                "alpha",
                "--model",
                "sonnet",
                "--permission-mode",
                "auto",
                "--remote-control",
                "--dangerously-load-development-channels",
                "server:claude-peers"
            ]
        );
    }

    #[test]
    fn load_reads_a_complete_spec_and_nothing_else() {
        let d = tempfile::tempdir().unwrap();
        spec::write_atomic(d.path(), &spec_of("alpha", "sonnet")).unwrap();
        let got = load(d.path(), "alpha").expect("a complete spec loads");
        assert_eq!(got.cwd, "C:/Projects/alpha");

        // Missing: positive control is the line above, same directory.
        assert_eq!(load(d.path(), "beta"), None);

        // Present but not complete (Opus): `write_atomic` refuses it, so write the bytes by hand
        // and check that `load` refuses it too.
        let opus = spec_of("gamma", "opus");
        let json = serde_json::to_vec_pretty(&opus).unwrap();
        std::fs::write(d.path().join("gamma.launch.json"), json).unwrap();
        assert!(
            spec::read(d.path(), "gamma").is_ok(),
            "the file is there and parses"
        );
        assert_eq!(load(d.path(), "gamma"), None, "an Opus spec is never used");

        // Not valid JSON.
        std::fs::write(d.path().join("delta.launch.json"), b"{not json").unwrap();
        assert_eq!(load(d.path(), "delta"), None);

        // A role that is not a plain identifier never reaches the filesystem.
        assert_eq!(load(d.path(), "../alpha"), None);
    }
}
