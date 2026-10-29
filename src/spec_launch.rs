// SPDX-License-Identifier: MIT OR Apache-2.0
//! A lane's launch spec (`<role>.launch.json`, written by `lane-restart`) as the planner's
//! preferred way back (the PM's ruling of 2026-10-14, option A): **spec first, the registry as the
//! flagged fallback**. One launch description for `lane-restart start` and for `restore`, so a lane
//! that is resumable by one path is resumable by the other with the same allowlist and model pin.
//!
//! Only a **complete** spec is used to launch (`SpecFile::check`: schema known, explicit non-Opus
//! model, a plain role and name, no `;`). An incomplete, unreadable or missing one is `None`, and the
//! planner then falls back to the registry record and says so on the entry.
//!
//! **One exception, so a deliberate stop survives logon:** a spec that records `suspended` or
//! `stopping` is returned (`suspended: true`) even when it is incomplete or cannot be fully read (a
//! newer schema, a missing field), and the planner then excludes the lane instead of falling back to
//! the registry record, which would relaunch it. A spec whose embedded role is not the requested one
//! is not used at all.
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
    /// The spec's state is `suspended` or `stopping`: `lane-restart stop` or `agentlife down` ended
    /// (or was ending) this lane on purpose. `restore` must leave it alone (a deliberate stop survives
    /// logon, including a stop whose final `suspended` write failed); only `lane-restart start`
    /// resumes it. When true, `cwd`/`argv` may be empty.
    pub suspended: bool,
}

/// Turns a spec into a [`SpecLaunch`]. Pure.
pub fn from_spec(s: &spec::SpecFile) -> SpecLaunch {
    let mut argv = vec![
        "--name".to_string(),
        s.name.to_string(),
        "--model".to_string(),
        s.model.clone(),
    ];
    if let Some(m) = &s.permission_mode {
        argv.push("--permission-mode".to_string());
        argv.push(m.as_str().to_string());
    }
    if s.remote_control {
        argv.push("--remote-control".to_string());
    }
    argv.extend(s.launch_args.iter().cloned());
    SpecLaunch {
        cwd: s.cwd.clone(),
        argv,
        suspended: matches!(
            s.state,
            spec::SpecState::Suspended | spec::SpecState::Stopping
        ),
    }
}

/// The role's complete spec in `lane_state_dir`, or `None` when there is none, it cannot be read,
/// it is not complete, or it belongs to another role. **Except** a spec that records a stop
/// (`suspended`/`stopping`): that is returned with `suspended: true` even when incomplete or only
/// partly readable, so the planner excludes the lane rather than relaunch it from the registry.
pub fn load(lane_state_dir: &Path, role: &str) -> Option<SpecLaunch> {
    match spec::read(lane_state_dir, role) {
        Ok(s) => {
            if s.role.as_str() != role {
                return None;
            }
            let launch = from_spec(&s);
            if s.check().is_err() && !launch.suspended {
                return None;
            }
            Some(launch)
        }
        Err(_) => records_a_stop(lane_state_dir, role).then(|| SpecLaunch {
            cwd: String::new(),
            argv: Vec::new(),
            suspended: true,
        }),
    }
}

/// A spec file that `spec::read` rejects (newer schema, a required field missing) but whose raw
/// JSON still says it is `suspended` or `stopping`, for this role.
fn records_a_stop(dir: &Path, role: &str) -> bool {
    if !lane_restart::relaunch::valid_identifier(role) {
        return false;
    }
    let Ok(bytes) = std::fs::read(dir.join(format!("{role}.launch.json"))) else {
        return false;
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    let named_here = v
        .get("role")
        .and_then(|r| r.as_str())
        .is_none_or(|r| r == role);
    named_here
        && matches!(
            v.get("state").and_then(|s| s.as_str()),
            Some("suspended" | "stopping")
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn spec_of(role: &str, model: &str) -> spec::SpecFile {
        spec::SpecFile {
            schema: spec::SCHEMA,
            role: lane_state::role::Role::new(role).unwrap(),
            name: lane_state::role::Role::new(role).unwrap(),
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
            written_by: lane_restart::spec::WrittenBy::Other("test".into()),
            stopped_session_id: None,
            stopped_pid_start_secs: None,
            stop_reason: None,
            stop_kind: None,
            stop_mac: None,
            tool_version: "test".into(),
            launch: None,
            wsl: None,
        }
    }

    // `agentlife stop` records the kind in the spec it suspends (board 216).
    #[test]
    fn a_down_records_the_stop_kind_in_the_suspended_spec() {
        use crate::down::{Facts, FsFacts};
        let d = tempfile::tempdir().unwrap();
        let mut run = spec_of("alpha", "sonnet");
        run.state = spec::SpecState::Running;
        spec::write_atomic(d.path(), &run).unwrap();
        let facts = FsFacts {
            lane_state_dir: d.path(),
        };
        facts
            .mark_suspended("alpha", "agentlife stop: test", spec::StopKind::Soft)
            .unwrap();
        let after = spec::read(d.path(), "alpha").unwrap();
        assert_eq!(after.state, spec::SpecState::Suspended);
        assert_eq!(after.stop_kind, Some(spec::StopKind::Soft));
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
    fn load_reports_a_suspended_spec_even_when_it_is_not_complete() {
        let d = tempfile::tempdir().unwrap();
        // complete + suspended
        spec::write_atomic(d.path(), &spec_of("alpha", "sonnet")).unwrap();
        assert!(load(d.path(), "alpha").unwrap().suspended);
        // complete + running: not suspended (the control)
        let mut run = spec_of("beta", "sonnet");
        run.state = spec::SpecState::Running;
        spec::write_atomic(d.path(), &run).unwrap();
        assert!(!load(d.path(), "beta").unwrap().suspended);
        // incomplete (Opus) + suspended: still reported, so it cannot fall back to the registry
        let opus = spec_of("gamma", "opus");
        std::fs::write(
            d.path().join("gamma.launch.json"),
            serde_json::to_vec_pretty(&opus).unwrap(),
        )
        .unwrap();
        assert!(load(d.path(), "gamma").unwrap().suspended);
        // stopping (the final suspended write failed): also a stop
        let mut stopping = spec_of("eps", "sonnet");
        stopping.state = spec::SpecState::Stopping;
        spec::write_atomic(d.path(), &stopping).unwrap();
        assert!(load(d.path(), "eps").unwrap().suspended);
        // a file `spec::read` rejects but whose raw JSON says suspended: still a stop
        std::fs::write(
            d.path().join("zeta.launch.json"),
            br#"{"role":"zeta","state":"suspended","schema":9999}"#,
        )
        .unwrap();
        assert!(
            spec::read(d.path(), "zeta").is_err(),
            "control: read rejects it"
        );
        assert!(load(d.path(), "zeta").unwrap().suspended);
        // another role's spec under this filename is not used
        let other = spec_of("someone-else", "sonnet");
        std::fs::write(
            d.path().join("theta.launch.json"),
            serde_json::to_vec_pretty(&other).unwrap(),
        )
        .unwrap();
        assert_eq!(load(d.path(), "theta"), None);
        // incomplete + running: None, as before
        let mut opus_run = spec_of("delta", "opus");
        opus_run.state = spec::SpecState::Running;
        std::fs::write(
            d.path().join("delta.launch.json"),
            serde_json::to_vec_pretty(&opus_run).unwrap(),
        )
        .unwrap();
        assert_eq!(load(d.path(), "delta"), None);
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
        let mut opus = spec_of("gamma", "opus");
        opus.state = spec::SpecState::Running; // a suspended one is reported, see the test above
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
