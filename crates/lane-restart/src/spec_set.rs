// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart specs set`: change other recorded launch fields across roles (PM [TASK]
//! fleet-spec-tooling, slice 3). `specs synapse-only` and `specs both-channels` own the channel flags;
//! this owns the rest: the model, the permission mode, remote control, and the extra launch arguments.
//!
//! Rules, each one a refusal (the role is left untouched):
//! * **No Opus** (CireSnave 2026-10-08, "I can't afford Opus."): `--model` is `sonnet` or `haiku` only.
//! * The permission mode is `default`, `acceptEdits` or `auto`: `bypassPermissions` is never launched.
//! * A launch argument is one token, and may not be a channel flag, `--model` or `--permission-mode`
//!   (each has its own command or option) nor `--dangerously-skip-permissions`.
//! * The result must pass `SpecFile::check`, which `spec::write_atomic` runs before it writes.
//!
//! One backup per spec, `<role>.launch.json.pre-set`, written once and never overwritten. The write is
//! `spec::write_atomic`, which signs a spec that records a soft stop, and the read-back here confirms the
//! stop kind still reads as it did (an unsigned soft stop reads as unclassified and no launcher starts it).
//! A running session is not touched: the change applies at its next launch.

use crate::relaunch::valid_identifier;
use crate::spec::{self, SpecFile};
use crate::spec_synapse::{RewriteOutcome, RewriteRow, DEV_CHANNELS, MCP_CONFIG, STRICT};
use chrono::{DateTime, Utc};
use std::path::Path;

const BACKUP_SUFFIX: &str = ".pre-set";

/// What to change. `None` and empty mean "leave as recorded".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetOptions {
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub remote_control: Option<bool>,
    pub add_args: Vec<String>,
    pub remove_args: Vec<String>,
}

/// Why the options themselves are refused, before any role is read.
pub fn validate(o: &SetOptions) -> Result<(), String> {
    if *o == SetOptions::default() {
        return Err("nothing to set: give --model, --permission-mode, --remote-control, --add-arg or --remove-arg".to_string());
    }
    if let Some(m) = &o.model {
        if !matches!(m.as_str(), "sonnet" | "haiku") {
            return Err(format!(
                "--model {m:?} refused: only an explicit sonnet or haiku is launched (no Opus)"
            ));
        }
    }
    if let Some(p) = &o.permission_mode {
        if !matches!(p.as_str(), "default" | "acceptEdits" | "auto") {
            return Err(format!(
                "--permission-mode {p:?} refused: default, acceptEdits or auto only (bypassPermissions is never launched)"
            ));
        }
    }
    for a in o.add_args.iter().chain(&o.remove_args) {
        if a.trim().is_empty() || a.chars().any(char::is_whitespace) {
            return Err(format!(
                "launch argument {a:?} must be one token with no spaces"
            ));
        }
        let channel = a == MCP_CONFIG
            || a == DEV_CHANNELS
            || a == STRICT
            || a.starts_with("server:")
            || a.starts_with("--mcp-config");
        if channel {
            return Err(format!(
                "{a:?} is a channel flag: use `specs synapse-only` or `specs both-channels`"
            ));
        }
        if matches!(
            a.as_str(),
            "--model" | "--permission-mode" | "--dangerously-skip-permissions"
        ) || a.starts_with("--model=")
            || a.starts_with("--permission-mode=")
        {
            return Err(format!(
                "{a:?} refused: use --model or --permission-mode, and bypassing permissions is never launched"
            ));
        }
    }
    if let Some(a) = o.add_args.iter().find(|a| o.remove_args.contains(a)) {
        return Err(format!("{a:?} is both added and removed"));
    }
    Ok(())
}

/// The recorded spec with `o` applied, and a line per change. Pure. `Err` when a removal names an
/// argument the spec does not carry (a typo must not look like success).
pub fn apply(s: &SpecFile, o: &SetOptions) -> Result<(SpecFile, Vec<String>), String> {
    let mut n = s.clone();
    let mut changes = Vec::new();
    if let Some(m) = &o.model {
        if &n.model != m {
            changes.push(format!("model {} -> {m}", n.model));
            n.model = m.clone();
        }
    }
    if let Some(p) = &o.permission_mode {
        let was = n
            .permission_mode
            .as_ref()
            .map_or("unset".to_string(), |m| m.as_str().to_string());
        if was != *p {
            changes.push(format!("permission mode {was} -> {p}"));
            n.permission_mode = Some(p.as_str().into());
        }
    }
    if let Some(r) = o.remote_control {
        if n.remote_control != r {
            changes.push(format!("remote control {} -> {r}", n.remote_control));
            n.remote_control = r;
        }
    }
    for a in &o.remove_args {
        let before = n.launch_args.len();
        n.launch_args.retain(|x| x != a);
        if n.launch_args.len() == before {
            return Err(format!("the spec carries no {a:?} to remove"));
        }
        changes.push(format!("removed {a}"));
    }
    for a in &o.add_args {
        if !n.launch_args.contains(a) {
            n.launch_args.push(a.clone());
            changes.push(format!("added {a}"));
        }
    }
    Ok((n, changes))
}

fn set_one(
    dir: &Path,
    role: &str,
    o: &SetOptions,
    dry_run: bool,
    now: DateTime<Utc>,
) -> RewriteOutcome {
    if !valid_identifier(role) {
        return RewriteOutcome::Refused(format!("not a plain role identifier: {role:?}"));
    }
    let before = match spec::read(dir, role) {
        Ok(s) => s,
        Err(e) => return RewriteOutcome::Refused(format!("no usable launch spec: {e}")),
    };
    let (mut after, changes) = match apply(&before, o) {
        Ok(x) => x,
        Err(why) => return RewriteOutcome::Refused(why),
    };
    if changes.is_empty() {
        return RewriteOutcome::Unchanged;
    }
    // The result must be launchable, checked before anything is written (also in a dry run).
    if let Err(e) = after.check() {
        return RewriteOutcome::Refused(format!("the result would not launch: {e}"));
    }
    if dry_run {
        return RewriteOutcome::WouldRewrite(changes);
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
    after.written_at = now;
    after.written_by = "set".into();
    if let Err(e) = spec::write_atomic(dir, &after) {
        return RewriteOutcome::Refused(format!("cannot write the spec: {e}"));
    }
    match spec::read(dir, role) {
        Ok(back) if back.stop_kind == before.stop_kind => RewriteOutcome::Rewrote(changes),
        Ok(back) => RewriteOutcome::Refused(format!(
            "written, but the stop kind now reads {:?}, not {:?}: restore {}",
            back.stop_kind,
            before.stop_kind,
            backup.display()
        )),
        Err(e) => RewriteOutcome::Refused(format!(
            "written, but it does not read back ({e}): restore {}",
            backup.display()
        )),
    }
}

/// One row per role, in the order given.
pub fn set_roles(
    dir: &Path,
    roles: &[String],
    o: &SetOptions,
    dry_run: bool,
    now: DateTime<Utc>,
) -> Vec<RewriteRow> {
    roles
        .iter()
        .map(|r| RewriteRow {
            role: r.clone(),
            outcome: set_one(dir, r, o, dry_run, now),
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
            RewriteOutcome::Rewrote(c) => {
                done += 1;
                out.push_str(&format!("{}\tset\t{}\n", r.role, c.join("; ")));
            }
            RewriteOutcome::WouldRewrite(c) => {
                done += 1;
                out.push_str(&format!("{}\twould set\t{}\n", r.role, c.join("; ")));
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
            "DRY RUN - nothing written ({done} would change, {unchanged} unchanged, {refused} refused)\n"
        ));
    } else {
        out.push_str(&format!(
            "CHANGED {done} spec(s), {unchanged} unchanged, {refused} refused; effective at each lane's next launch\n"
        ));
    }
    out
}

#[cfg(test)]
mod tests;
