// SPDX-License-Identifier: MIT OR Apache-2.0

//! The push channels a lane is launched with, and `lane-restart specs both-channels`.
//!
//! Every lane is launched with `--mcp-config <role>.mcp.json --dangerously-load-development-channels
//! server:claude-peers server:synapse` (the flags antspawn runs with), so a message sent over either
//! transport reaches it. Measured from the `claude.exe` command lines on 2026-10-31, 3 lanes loaded only
//! Synapse, 10 only claude-peers and 14 both: a message to a lane on the other transport was never seen.
//!
//! `check` is the one definition of "complete"; the tests run every spec this crate builds through it.
//! `rewrite_roles` brings recorded specs to the full form (PM only, like `specs synapse-only`), keeping
//! one backup per spec. A running session is not touched: the change applies at its next launch.

use crate::relaunch::valid_identifier;
use crate::spec;
use crate::spec_synapse::{
    config_names_synapse, mcp_config_arg, RewriteOutcome, RewriteRow, DEV_CHANNELS, MCP_CONFIG,
    PEERS, STRICT, SYNAPSE,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::path::Path;

const BACKUP_SUFFIX: &str = ".pre-both-channels";

/// Which of the three launch flags a `claude` command line carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Channels {
    pub mcp_config: bool,
    pub peers: bool,
    pub synapse: bool,
}

impl Channels {
    pub fn of(args: &[String]) -> Channels {
        // The `server:` names run from the dev-channels flag to the next token that is not one.
        let names: &[String] = match args.iter().position(|a| a == DEV_CHANNELS) {
            Some(f) => {
                let end = args[f + 1..]
                    .iter()
                    .position(|a| !a.starts_with("server:"))
                    .map_or(args.len(), |n| f + 1 + n);
                &args[f + 1..end]
            }
            None => &[],
        };
        Channels {
            mcp_config: mcp_config_arg(args).is_some(),
            peers: names.iter().any(|a| a == PEERS),
            synapse: names.iter().any(|a| a == SYNAPSE),
        }
    }

    pub fn complete(self) -> bool {
        self.mcp_config && self.peers && self.synapse
    }

    /// `mcp+peers+synapse`, with `-` for each flag that is missing (`mcp+-+synapse`).
    pub fn text(self) -> String {
        format!(
            "{}+{}+{}",
            if self.mcp_config { "mcp" } else { "-" },
            if self.peers { "peers" } else { "-" },
            if self.synapse { "synapse" } else { "-" }
        )
    }
}

/// `Ok` when the arguments carry `--mcp-config` and both channels; otherwise what is missing.
pub fn check(args: &[String]) -> Result<(), String> {
    let c = Channels::of(args);
    let mut missing = Vec::new();
    if !c.mcp_config {
        missing.push(MCP_CONFIG);
    }
    if !c.peers {
        missing.push(PEERS);
    }
    if !c.synapse {
        missing.push(SYNAPSE);
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("launch args lack {}", missing.join(", ")))
    }
}

/// The arguments in the full form. Pure. Existing `server:` names are kept in place and the missing
/// ones are appended; a missing dev-channels flag is added at the end; a missing `--mcp-config` is
/// inserted just before the dev-channels flag, using `mcp_path`. `Err` when the spec asks for
/// `--strict-mcp-config`: that stops the user-scope claude-peers server from loading at all.
pub fn full_args(args: &[String], mcp_path: &str) -> Result<Vec<String>, String> {
    if args.iter().any(|a| a == STRICT) {
        return Err(
            "the spec has --strict-mcp-config, which keeps the claude-peers server from loading"
                .to_string(),
        );
    }
    let mut out: Vec<String> = args.to_vec();
    if mcp_config_arg(&out).is_none() {
        let at = out
            .iter()
            .position(|a| a == DEV_CHANNELS)
            .unwrap_or(out.len());
        out.splice(at..at, [MCP_CONFIG.to_string(), mcp_path.to_string()]);
    }
    let flag = match out.iter().position(|a| a == DEV_CHANNELS) {
        Some(f) => f,
        None => {
            out.push(DEV_CHANNELS.to_string());
            out.len() - 1
        }
    };
    let end = out[flag + 1..]
        .iter()
        .position(|a| !a.starts_with("server:"))
        .map_or(out.len(), |n| flag + 1 + n);
    let have = Channels::of(&out);
    let mut add = Vec::new();
    if !have.peers {
        add.push(PEERS.to_string());
    }
    if !have.synapse {
        add.push(SYNAPSE.to_string());
    }
    out.splice(end..end, add);
    Ok(out)
}

/// A drive-letter path with backslashes, as the existing specs spell it (C:, then Projects, .lane-state, x.mcp.json);
/// any other path is returned as it is.
fn windows_slashes(p: &str) -> String {
    let b = p.as_bytes();
    if b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        p.replace('/', "\\")
    } else {
        p.to_string()
    }
}

fn rewrite_one(dir: &Path, role: &str, dry_run: bool, now: DateTime<Utc>) -> RewriteOutcome {
    if !valid_identifier(role) {
        return RewriteOutcome::Refused(format!("not a plain role identifier: {role:?}"));
    }
    let mut s = match spec::read(dir, role) {
        Ok(s) => s,
        Err(e) => return RewriteOutcome::Refused(format!("no usable launch spec: {e}")),
    };
    // The role's own config when the spec has none; the file must exist and name the synapse server
    // (otherwise `server:synapse` is an inert channel name).
    let own = dir.join(format!("{role}.mcp.json"));
    let cfg = match mcp_config_arg(&s.launch_args) {
        Some(c) => c.to_string(),
        None => windows_slashes(&own.to_string_lossy()),
    };
    if let Err(why) = config_names_synapse(&cfg) {
        return RewriteOutcome::Refused(why);
    }
    let new_args = match full_args(&s.launch_args, &cfg) {
        Ok(a) => a,
        Err(why) => return RewriteOutcome::Refused(why),
    };
    if new_args == s.launch_args {
        return RewriteOutcome::Unchanged;
    }
    if dry_run {
        return RewriteOutcome::WouldRewrite(new_args);
    }
    let backup = dir.join(format!("{role}{}{BACKUP_SUFFIX}", spec::FILE_SUFFIX));
    if !backup.exists() {
        let from = dir.join(format!("{role}{}", spec::FILE_SUFFIX));
        if let Err(e) = std::fs::copy(&from, &backup) {
            return RewriteOutcome::Refused(format!(
                "cannot write the backup {}: {e}",
                backup.display()
            ));
        }
    }
    s.launch_args = new_args.clone();
    s.written_at = now;
    s.written_by = "both-channels".into();
    match spec::write_atomic(dir, &s) {
        Ok(()) => RewriteOutcome::Rewrote(new_args),
        Err(e) => RewriteOutcome::Refused(format!("cannot write the spec: {e}")),
    }
}

/// One row per role, in the order given.
pub fn rewrite_roles(
    dir: &Path,
    roles: &[String],
    dry_run: bool,
    now: DateTime<Utc>,
) -> Vec<RewriteRow> {
    roles
        .iter()
        .map(|r| RewriteRow {
            role: r.clone(),
            outcome: rewrite_one(dir, r, dry_run, now),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn check_names_each_missing_flag_and_passes_the_full_form() {
        let full = v(&["--mcp-config", "m.json", DEV_CHANNELS, PEERS, SYNAPSE]);
        assert_eq!(check(&full), Ok(()));
        assert!(Channels::of(&full).complete());
        assert!(!Channels::of(&v(&["--mcp-config", "m", DEV_CHANNELS, PEERS])).complete());
        // control: dropping each of the three in turn fails and names exactly it
        for (args, missing) in [
            (v(&[DEV_CHANNELS, PEERS, SYNAPSE]), MCP_CONFIG),
            (v(&["--mcp-config", "m.json", DEV_CHANNELS, SYNAPSE]), PEERS),
            (v(&["--mcp-config", "m.json", DEV_CHANNELS, PEERS]), SYNAPSE),
        ] {
            let e = check(&args).unwrap_err();
            assert!(e.ends_with(missing), "{e}");
        }
        // the names only count after the dev-channels flag and before the next non-server token
        let stray = v(&[
            "--mcp-config",
            "m.json",
            PEERS,
            SYNAPSE,
            DEV_CHANNELS,
            "--verbose",
        ]);
        assert!(check(&stray).is_err());
    }

    #[test]
    fn the_status_text_shows_the_three_flags() {
        assert_eq!(Channels::of(&v(&["claude.exe"])).text(), "-+-+-");
        assert_eq!(
            Channels::of(&v(&["--mcp-config", "m", DEV_CHANNELS, SYNAPSE])).text(),
            "mcp+-+synapse"
        );
        assert_eq!(Channels::of(&v(&[DEV_CHANNELS, PEERS])).text(), "-+peers+-");
    }

    #[test]
    fn full_args_completes_each_measured_shape_and_is_idempotent() {
        let p = "S/x.mcp.json";
        // agentlife / overmind / synapse: Synapse only, config present
        let a = full_args(&v(&[DEV_CHANNELS, SYNAPSE, MCP_CONFIG, "S/x.mcp.json"]), p).unwrap();
        assert_eq!(
            a,
            v(&[DEV_CHANNELS, SYNAPSE, PEERS, MCP_CONFIG, "S/x.mcp.json"])
        );
        // persistant and nine more: claude-peers only, no config
        let b = full_args(&v(&[DEV_CHANNELS, PEERS]), p).unwrap();
        assert_eq!(b, v(&[MCP_CONFIG, p, DEV_CHANNELS, PEERS, SYNAPSE]));
        // no dev-channels flag at all
        let c = full_args(&v(&["--model", "sonnet"]), p).unwrap();
        assert_eq!(
            c,
            v(&[
                "--model",
                "sonnet",
                MCP_CONFIG,
                p,
                DEV_CHANNELS,
                PEERS,
                SYNAPSE
            ])
        );
        // another channel is kept; the existing config is not replaced
        let d = full_args(
            &v(&[MCP_CONFIG, "other.json", DEV_CHANNELS, "server:x", PEERS]),
            p,
        )
        .unwrap();
        assert_eq!(
            d,
            v(&[
                MCP_CONFIG,
                "other.json",
                DEV_CHANNELS,
                "server:x",
                PEERS,
                SYNAPSE
            ])
        );
        for out in [&a, &b, &c, &d] {
            assert_eq!(check(out), Ok(()), "{out:?}");
            assert_eq!(&full_args(out, p).unwrap(), out, "not idempotent");
        }
        // strict config is refused, not rewritten
        assert!(full_args(&v(&[STRICT, DEV_CHANNELS, SYNAPSE]), p).is_err());
    }

    fn write_spec(dir: &Path, role: &str, args: &[&str]) {
        let s = spec::SpecFile {
            schema: spec::SCHEMA,
            role: lane_state::role::Role::new(role).unwrap(),
            name: lane_state::role::Role::new(role).unwrap(),
            cwd: format!("C:/Projects/{role}"),
            model: "sonnet".into(),
            permission_mode: None,
            remote_control: false,
            launch_args: v(args),
            handoff_path: None,
            handoff_mtime: None,
            state: spec::SpecState::Running,
            written_at: Utc::now(),
            written_by: "test".into(),
            stopped_session_id: None,
            stopped_pid_start_secs: None,
            stop_reason: None,
            stop_kind: None,
            stop_mac: None,
            tool_version: "test".into(),
            launch: None,
            wsl: None,
        };
        spec::write_atomic(dir, &s).unwrap();
    }

    fn write_cfg(dir: &Path, role: &str, with_synapse: bool) -> String {
        let p = dir.join(format!("{role}.mcp.json"));
        let body = if with_synapse {
            r#"{"mcpServers":{"synapse":{}}}"#
        } else {
            r#"{"mcpServers":{}}"#
        };
        std::fs::write(&p, body).unwrap();
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn rewrite_roles_brings_a_peers_only_spec_to_the_full_form_with_one_backup() {
        let d = tempfile::tempdir().unwrap();
        write_spec(d.path(), "pa", &[DEV_CHANNELS, PEERS]);
        let cfg = windows_slashes(&write_cfg(d.path(), "pa", true));
        let now = Utc::now();
        let roles = vec!["pa".to_string()];
        let dry = rewrite_roles(d.path(), &roles, true, now);
        assert!(
            matches!(dry[0].outcome, RewriteOutcome::WouldRewrite(_)),
            "{dry:?}"
        );
        assert_eq!(
            spec::read(d.path(), "pa").unwrap().launch_args,
            v(&[DEV_CHANNELS, PEERS])
        );
        let done = rewrite_roles(d.path(), &roles, false, now);
        assert!(
            matches!(done[0].outcome, RewriteOutcome::Rewrote(_)),
            "{done:?}"
        );
        let got = spec::read(d.path(), "pa").unwrap().launch_args;
        assert_eq!(got, v(&[MCP_CONFIG, &cfg, DEV_CHANNELS, PEERS, SYNAPSE]));
        assert_eq!(check(&got), Ok(()));
        assert!(d.path().join("pa.launch.json.pre-both-channels").exists());
        // a second run changes nothing and leaves the first backup
        let again = rewrite_roles(d.path(), &roles, false, now);
        assert_eq!(again[0].outcome, RewriteOutcome::Unchanged);
    }

    #[test]
    fn rewrite_roles_refuses_without_a_synapse_config_and_leaves_the_spec_alone() {
        let d = tempfile::tempdir().unwrap();
        write_spec(d.path(), "nocfg", &[DEV_CHANNELS, PEERS]);
        write_spec(d.path(), "nosyn", &[DEV_CHANNELS, PEERS]);
        write_cfg(d.path(), "nosyn", false);
        for role in ["nocfg", "nosyn", "Bad/Role"] {
            let r = rewrite_roles(d.path(), &[role.to_string()], false, Utc::now());
            assert!(
                matches!(r[0].outcome, RewriteOutcome::Refused(_)),
                "{role}: {r:?}"
            );
        }
        assert_eq!(
            spec::read(d.path(), "nocfg").unwrap().launch_args,
            v(&[DEV_CHANNELS, PEERS])
        );
        assert!(!d
            .path()
            .join("nocfg.launch.json.pre-both-channels")
            .exists());
    }
}
