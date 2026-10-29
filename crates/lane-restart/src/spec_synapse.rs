// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart specs synapse-only` - rewrite a lane's recorded `launch_args` so its next launch
//! loads the Synapse channel and not the claude-peers one (docs/SYNAPSE-ONLY-CUTOVER.md, step 1a).
//! PM only, like `specs import` and `specs retire`.
//!
//! What it changes, and nothing else: the `server:` names after `--dangerously-load-development-channels`
//! become `server:synapse` (an existing `server:claude-peers` is dropped, other names are kept), and with
//! `--strict` a `--strict-mcp-config` is put before the dev-channels flag so the user-scope claude-peers
//! server does not load either. A lane is refused, untouched, when its spec has no `--mcp-config`, when
//! that file is missing, unreadable or names no `synapse` server (its channel name would be inert), or
//! when it has no dev-channels flag. The previous spec is copied once to `<role>.launch.json.pre-synapse`
//! and never overwritten, so rolling back is copying that file back. The change takes effect at the lane's
//! next launch; a running session is not touched.

use crate::relaunch::valid_identifier;
use crate::spec;
use chrono::{DateTime, Utc};
use std::path::Path;

const DEV_CHANNELS: &str = "--dangerously-load-development-channels";
const MCP_CONFIG: &str = "--mcp-config";
const STRICT: &str = "--strict-mcp-config";
const PEERS: &str = "server:claude-peers";
const SYNAPSE: &str = "server:synapse";
const BACKUP_SUFFIX: &str = ".pre-synapse";

#[derive(Debug, PartialEq, Eq)]
pub enum RewriteOutcome {
    /// Rewritten; the new arguments.
    Rewrote(Vec<String>),
    /// `--dry-run`: these arguments would be written.
    WouldRewrite(Vec<String>),
    /// Already in the requested form.
    Unchanged,
    /// Not rewritten; the reason is the whole answer.
    Refused(String),
}

#[derive(Debug, PartialEq, Eq)]
pub struct RewriteRow {
    pub role: String,
    pub outcome: RewriteOutcome,
}

/// The path after `--mcp-config`, if the arguments carry one.
fn mcp_config_arg(args: &[String]) -> Option<&str> {
    let i = args.iter().position(|a| a == MCP_CONFIG)?;
    args.get(i + 1).map(String::as_str)
}

/// `Ok` when the file parses as JSON and `mcpServers` has a `synapse` entry. Values are never read
/// into the message: the file can hold a home path and role names, and nothing here needs more.
fn config_names_synapse(path: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| format!("{path} is not valid JSON"))?;
    match v.get("mcpServers").and_then(|m| m.get("synapse")) {
        Some(_) => Ok(()),
        None => Err(format!("{path} has no mcpServers.synapse")),
    }
}

/// The arguments in Synapse-only form. Pure. `Err` when there is no dev-channels flag to edit.
///
/// The `server:` names run from the flag to the next token that is not one. They are replaced by the
/// same list without `server:claude-peers`, with `server:synapse` added once, so the flag keeps its
/// place (it goes last in a launch) and anything after the names is untouched.
pub fn rewrite_args(args: &[String], strict: bool) -> Result<Vec<String>, String> {
    let flag = args
        .iter()
        .position(|a| a == DEV_CHANNELS)
        .ok_or("no --dangerously-load-development-channels flag in the spec")?;
    let end = args[flag + 1..]
        .iter()
        .position(|a| !a.starts_with("server:"))
        .map_or(args.len(), |n| flag + 1 + n);
    let mut names: Vec<&str> = args[flag + 1..end]
        .iter()
        .map(String::as_str)
        .filter(|n| *n != PEERS)
        .collect();
    if !names.contains(&SYNAPSE) {
        names.push(SYNAPSE);
    }
    let mut out: Vec<String> = args[..flag].to_vec();
    if strict && !out.iter().any(|a| a == STRICT) {
        out.push(STRICT.to_string());
    }
    out.push(DEV_CHANNELS.to_string());
    out.extend(names.into_iter().map(str::to_string));
    out.extend(args[end..].iter().cloned());
    Ok(out)
}

fn rewrite_one(
    dir: &Path,
    role: &str,
    strict: bool,
    dry_run: bool,
    now: DateTime<Utc>,
) -> RewriteOutcome {
    if !valid_identifier(role) {
        return RewriteOutcome::Refused(format!("not a plain role identifier: {role:?}"));
    }
    let mut s = match spec::read(dir, role) {
        Ok(s) => s,
        Err(e) => return RewriteOutcome::Refused(format!("no usable launch spec: {e}")),
    };
    let Some(cfg) = mcp_config_arg(&s.launch_args) else {
        return RewriteOutcome::Refused(
            "the spec has no --mcp-config: the Synapse server would not load".to_string(),
        );
    };
    if let Err(why) = config_names_synapse(cfg) {
        return RewriteOutcome::Refused(why);
    }
    let new_args = match rewrite_args(&s.launch_args, strict) {
        Ok(a) => a,
        Err(why) => return RewriteOutcome::Refused(why),
    };
    if new_args == s.launch_args {
        return RewriteOutcome::Unchanged;
    }
    if dry_run {
        return RewriteOutcome::WouldRewrite(new_args);
    }
    // One backup, written once, never replaced: it is the spec as it was before the first rewrite.
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
    s.written_by = "synapse-only".into();
    match spec::write_atomic(dir, &s) {
        Ok(()) => RewriteOutcome::Rewrote(new_args),
        Err(e) => RewriteOutcome::Refused(format!("cannot write the spec: {e}")),
    }
}

/// One row per role, in the order given.
pub fn rewrite_roles(
    dir: &Path,
    roles: &[String],
    strict: bool,
    dry_run: bool,
    now: DateTime<Utc>,
) -> Vec<RewriteRow> {
    roles
        .iter()
        .map(|r| RewriteRow {
            role: r.clone(),
            outcome: rewrite_one(dir, r, strict, dry_run, now),
        })
        .collect()
}

/// The report: the first and the last line say whether anything was written.
pub fn render(rows: &[RewriteRow], dry_run: bool) -> String {
    let mut out = String::new();
    if dry_run {
        out.push_str("DRY RUN - nothing written\n");
    }
    let (mut done, mut unchanged, mut refused) = (0, 0, 0);
    for r in rows {
        match &r.outcome {
            RewriteOutcome::Rewrote(a) => {
                done += 1;
                out.push_str(&format!("{}\trewrote\t{}\n", r.role, a.join(" ")));
            }
            RewriteOutcome::WouldRewrite(a) => {
                done += 1;
                out.push_str(&format!("{}\twould rewrite\t{}\n", r.role, a.join(" ")));
            }
            RewriteOutcome::Unchanged => {
                unchanged += 1;
                out.push_str(&format!("{}\tunchanged\n", r.role));
            }
            RewriteOutcome::Refused(why) => {
                refused += 1;
                out.push_str(&format!("{}\trefused\t{why}\n", r.role));
            }
        }
    }
    if dry_run {
        out.push_str(&format!(
            "DRY RUN - nothing written ({done} would be rewritten, {unchanged} unchanged, {refused} refused)\n"
        ));
    } else {
        out.push_str(&format!(
            "REWROTE {done} spec(s), {unchanged} unchanged, {refused} refused; effective at each lane's next launch\n"
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
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

    fn mcp_file(dir: &Path, name: &str, servers: &str) -> String {
        let p = dir.join(name);
        std::fs::write(&p, format!(r#"{{"mcpServers":{{{servers}}}}}"#)).unwrap();
        p.to_string_lossy().into_owned()
    }

    fn run(d: &Path, roles: &[&str], strict: bool, dry: bool) -> Vec<RewriteRow> {
        let roles: Vec<String> = roles.iter().map(|s| s.to_string()).collect();
        rewrite_roles(d, &roles, strict, dry, Utc::now())
    }

    #[test]
    fn the_names_after_the_flag_become_synapse_only_and_the_flag_keeps_its_place() {
        let peers_only = v(&["--mcp-config", "m.json", DEV_CHANNELS, PEERS]);
        assert_eq!(
            rewrite_args(&peers_only, false).unwrap(),
            v(&["--mcp-config", "m.json", DEV_CHANNELS, SYNAPSE])
        );
        let both = v(&[DEV_CHANNELS, PEERS, SYNAPSE]);
        assert_eq!(
            rewrite_args(&both, false).unwrap(),
            v(&[DEV_CHANNELS, SYNAPSE])
        );
        // another server name survives, a trailing argument is untouched, synapse is not doubled
        let mixed = v(&[DEV_CHANNELS, "server:other", PEERS, SYNAPSE, "--verbose"]);
        assert_eq!(
            rewrite_args(&mixed, false).unwrap(),
            v(&[DEV_CHANNELS, "server:other", SYNAPSE, "--verbose"])
        );
        let done = v(&[DEV_CHANNELS, SYNAPSE]);
        assert_eq!(rewrite_args(&done, false).unwrap(), done, "idempotent");
        assert!(rewrite_args(&v(&["--mcp-config", "m.json"]), false).is_err());
    }

    #[test]
    fn strict_puts_the_flag_before_the_dev_channels_flag_once() {
        let a = v(&["--mcp-config", "m.json", DEV_CHANNELS, PEERS]);
        let once = rewrite_args(&a, true).unwrap();
        assert_eq!(
            once,
            v(&["--mcp-config", "m.json", STRICT, DEV_CHANNELS, SYNAPSE])
        );
        assert_eq!(
            rewrite_args(&once, true).unwrap(),
            once,
            "no second --strict-mcp-config"
        );
    }

    #[test]
    fn a_lane_with_a_synapse_config_is_rewritten_with_one_backup_and_a_dry_run_writes_nothing() {
        let d = tempdir().unwrap();
        let cfg = mcp_file(d.path(), "a.mcp.json", r#""synapse":{}"#);
        write_spec(
            d.path(),
            "a",
            &["--mcp-config", &cfg, DEV_CHANNELS, PEERS, SYNAPSE],
        );
        let before = std::fs::read_to_string(d.path().join("a.launch.json")).unwrap();

        let dry = run(d.path(), &["a"], false, true);
        assert!(matches!(dry[0].outcome, RewriteOutcome::WouldRewrite(_)));
        assert_eq!(
            std::fs::read_to_string(d.path().join("a.launch.json")).unwrap(),
            before
        );
        assert!(!d.path().join("a.launch.json.pre-synapse").exists());

        let rows = run(d.path(), &["a"], false, false);
        assert_eq!(
            rows[0].outcome,
            RewriteOutcome::Rewrote(v(&["--mcp-config", &cfg, DEV_CHANNELS, SYNAPSE]))
        );
        let now = spec::read(d.path(), "a").unwrap();
        assert_eq!(
            now.launch_args,
            v(&["--mcp-config", &cfg, DEV_CHANNELS, SYNAPSE])
        );
        assert_eq!(now.written_by.as_str(), "synapse-only");
        let backup = d.path().join("a.launch.json.pre-synapse");
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            before,
            "the backup is the old spec"
        );

        // A second run changes nothing and leaves the original backup as it was.
        assert_eq!(
            run(d.path(), &["a"], false, false)[0].outcome,
            RewriteOutcome::Unchanged
        );
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), before);
    }

    #[test]
    fn a_lane_without_a_usable_synapse_config_is_refused_and_untouched() {
        let d = tempdir().unwrap();
        let no_synapse = mcp_file(d.path(), "p.mcp.json", r#""claude-peers":{}"#);
        write_spec(d.path(), "none", &[DEV_CHANNELS, PEERS]);
        write_spec(
            d.path(),
            "wrong",
            &["--mcp-config", &no_synapse, DEV_CHANNELS, PEERS],
        );
        write_spec(
            d.path(),
            "gone",
            &["--mcp-config", "Z:/no/such.json", DEV_CHANNELS, PEERS],
        );
        write_spec(
            d.path(),
            "noflag",
            &[
                "--mcp-config",
                &mcp_file(d.path(), "n.json", r#""synapse":{}"#),
            ],
        );
        let before: Vec<String> = ["none", "wrong", "gone", "noflag"]
            .iter()
            .map(|r| std::fs::read_to_string(d.path().join(format!("{r}.launch.json"))).unwrap())
            .collect();
        let rows = run(
            d.path(),
            &["none", "wrong", "gone", "noflag", "nosuch", "../x"],
            false,
            false,
        );
        let why = |i: usize| match &rows[i].outcome {
            RewriteOutcome::Refused(w) => w.clone(),
            o => panic!("row {i} not refused: {o:?}"),
        };
        assert!(why(0).contains("no --mcp-config"), "{}", why(0));
        assert!(why(1).contains("no mcpServers.synapse"), "{}", why(1));
        assert!(why(2).contains("cannot read"), "{}", why(2));
        assert!(why(3).contains("no --dangerously"), "{}", why(3));
        assert!(why(4).contains("no usable launch spec"), "{}", why(4));
        assert!(why(5).contains("plain role identifier"), "{}", why(5));
        for (i, r) in ["none", "wrong", "gone", "noflag"].iter().enumerate() {
            let now = std::fs::read_to_string(d.path().join(format!("{r}.launch.json"))).unwrap();
            assert_eq!(now, before[i], "{r} untouched");
            assert!(!d
                .path()
                .join(format!("{r}.launch.json.pre-synapse"))
                .exists());
        }
    }

    #[test]
    fn the_report_says_first_and_last_whether_anything_was_written() {
        let d = tempdir().unwrap();
        let cfg = mcp_file(d.path(), "a.mcp.json", r#""synapse":{}"#);
        write_spec(d.path(), "a", &["--mcp-config", &cfg, DEV_CHANNELS, PEERS]);
        let dry = render(&run(d.path(), &["a"], false, true), true);
        assert_eq!(dry.lines().next(), Some("DRY RUN - nothing written"));
        assert_eq!(
            dry.lines().last(),
            Some("DRY RUN - nothing written (1 would be rewritten, 0 unchanged, 0 refused)")
        );
        let real = render(&run(d.path(), &["a"], false, false), false);
        assert!(real.contains("a\trewrote\t"), "{real}");
        assert!(
            real.lines()
                .last()
                .unwrap()
                .starts_with("REWROTE 1 spec(s)"),
            "{real}"
        );
    }
}
