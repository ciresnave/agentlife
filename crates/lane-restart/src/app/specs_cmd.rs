// SPDX-License-Identifier: MIT OR Apache-2.0

//! The four `lane-restart specs` commands: `import`, `retire`, `synapse-only` and `both-channels`.
//! Three are PM only even as a dry run; `both-channels --dry-run` writes nothing and any caller may
//! read it. All four write `restart.log` rows.
//!
//! Moved out of `app.rs` unchanged apart from reading the caller and the process facts, and
//! writing their text and ledger rows, through the [`Ctx`] (thin-main PR 9).

use super::ctx::Ctx;
use crate::start;
use std::process::ExitCode;

/// `lane-restart specs import [--role R] [--dry-run] [--verbose]` - PM only.
pub(super) fn import(
    ctx: &mut Ctx,
    role: Option<String>,
    dry_run: bool,
    verbose: bool,
) -> ExitCode {
    use crate::spec_import::{import_specs, ImportOutcome};
    let caller = ctx.verified_caller();
    let caller_role = caller.as_ref().ok().cloned();
    let requested_by = caller_role
        .clone()
        .unwrap_or_else(|| "unverified".to_string());
    let target = role.clone().unwrap_or_else(|| "*".to_string());
    if caller_role.as_deref() != Some(start::PM_ROLE) {
        let why = match caller {
            Ok(r) => format!("only the PM session may import specs; this is running under {r:?}"),
            Err(e) => format!(
                "only the PM session may import specs, and the caller could not be verified: {e}"
            ),
        };
        ctx.errln(&format!("lane-restart specs import: refused - {why}"));
        ctx.log_ledger(
            &requested_by,
            &target,
            "import",
            caller_role.as_deref(),
            None,
            None,
            &format!("refused: {why}"),
            false,
        );
        return ExitCode::FAILURE;
    }
    let state = ctx.state_dir.clone();
    let facts = ctx.facts();
    let rows = import_specs(
        &state,
        role.as_deref(),
        &*facts,
        &crate::relaunch::policy_default_model(&state),
        dry_run,
        chrono::Utc::now(),
    );
    let mut refused_named = false;
    for r in &rows {
        match &r.outcome {
            ImportOutcome::Written(s) => {
                ctx.log_ledger(
                    &requested_by,
                    &r.role,
                    "import",
                    caller_role.as_deref(),
                    None,
                    None,
                    &format!("imported a launch spec from the lane state file (state={s:?})"),
                    true,
                );
            }
            ImportOutcome::WouldWrite(_) | ImportOutcome::AlreadyHasSpec => {}
            ImportOutcome::Refused(_) => refused_named |= role.is_some(),
        }
    }
    ctx.out(&crate::spec_import::render(&rows, dry_run, verbose));
    if refused_named {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `lane-restart specs retire --role R... [--dry-run]` - PM only. Moves, never deletes.
pub(super) fn retire(ctx: &mut Ctx, roles: Vec<String>, dry_run: bool) -> ExitCode {
    use crate::spec_retire::{render, retire_roles, RetireOutcome};
    let caller = ctx.verified_caller();
    let caller_role = caller.as_ref().ok().cloned();
    let requested_by = caller_role
        .clone()
        .unwrap_or_else(|| "unverified".to_string());
    if caller_role.as_deref() != Some(start::PM_ROLE) {
        let why = match caller {
            Ok(r) => format!("only the PM session may retire roles; this is running under {r:?}"),
            Err(e) => format!(
                "only the PM session may retire roles, and the caller could not be verified: {e}"
            ),
        };
        ctx.errln(&format!("lane-restart specs retire: refused - {why}"));
        ctx.log_ledger(
            &requested_by,
            &roles.join(","),
            "retire",
            caller_role.as_deref(),
            None,
            None,
            &format!("refused: {why}"),
            false,
        );
        return ExitCode::FAILURE;
    }
    let state = ctx.state_dir.clone();
    let facts = ctx.facts();
    let rows = retire_roles(&state, &roles, &*facts, dry_run, chrono::Utc::now());
    let mut refused = false;
    for r in &rows {
        match &r.outcome {
            RetireOutcome::Moved(files) => ctx.log_ledger(
                &requested_by,
                &r.role,
                "retire",
                caller_role.as_deref(),
                None,
                None,
                &format!("moved {} to retired/", files.join(", ")),
                true,
            ),
            RetireOutcome::WouldMove(_) => {}
            RetireOutcome::Refused(why) => {
                refused = true;
                ctx.log_ledger(
                    &requested_by,
                    &r.role,
                    "retire",
                    caller_role.as_deref(),
                    None,
                    None,
                    &format!("refused: {why}"),
                    false,
                );
            }
        }
    }
    ctx.out(&render(&rows, dry_run));
    if refused {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// The roles a rewrite covers: the ones named, or (`--all`) every role with a recorded launch spec.
fn resolve(ctx: &Ctx, roles: crate::cli::Roles) -> Vec<String> {
    match roles {
        crate::cli::Roles::Named(v) => v,
        crate::cli::Roles::All => crate::spec_audit::roles(&ctx.state_dir),
    }
}

/// After an `--all` rewrite: which roles were skipped as refused and why, or that none were. The
/// command exits 0 only when this says none.
fn refused_summary(rows: &[crate::spec_synapse::RewriteRow]) -> String {
    use crate::spec_synapse::RewriteOutcome;
    let skipped: Vec<String> = rows
        .iter()
        .filter_map(|r| match &r.outcome {
            RewriteOutcome::Refused(why) => Some(format!("{} ({why})", r.role)),
            _ => None,
        })
        .collect();
    if skipped.is_empty() {
        format!(
            "ALL {} role(s) handled, none refused
",
            rows.len()
        )
    } else {
        format!(
            "SKIPPED {} of {} role(s) as refused, exit 1: {}
",
            skipped.len(),
            rows.len(),
            skipped.join("; ")
        )
    }
}

/// `lane-restart specs synapse-only --role R... [--strict] [--dry-run]` - PM only. Rewrites a spec's
/// `launch_args` to the Synapse-only form (docs/SYNAPSE-ONLY-CUTOVER.md); one backup, never overwritten.
pub(super) fn synapse_only(
    ctx: &mut Ctx,
    roles: crate::cli::Roles,
    strict: bool,
    dry_run: bool,
) -> ExitCode {
    use crate::spec_synapse::{render, rewrite_roles, RewriteOutcome};
    let all = roles == crate::cli::Roles::All;
    let roles = resolve(ctx, roles);
    let caller = ctx.verified_caller();
    let caller_role = caller.as_ref().ok().cloned();
    let requested_by = caller_role
        .clone()
        .unwrap_or_else(|| "unverified".to_string());
    if caller_role.as_deref() != Some(start::PM_ROLE) {
        let why = match caller {
            Ok(r) => format!("only the PM session may rewrite specs; this is running under {r:?}"),
            Err(e) => format!(
                "only the PM session may rewrite specs, and the caller could not be verified: {e}"
            ),
        };
        ctx.errln(&format!("lane-restart specs synapse-only: refused - {why}"));
        ctx.log_ledger(
            &requested_by,
            &roles.join(","),
            "synapse-only",
            caller_role.as_deref(),
            None,
            None,
            &format!("refused: {why}"),
            false,
        );
        return ExitCode::FAILURE;
    }
    let state = ctx.state_dir.clone();
    let rows = rewrite_roles(&state, &roles, strict, dry_run, chrono::Utc::now());
    let mut refused = false;
    for r in &rows {
        let (outcome, acted) = match &r.outcome {
            RewriteOutcome::Rewrote(a) => (format!("rewrote launch_args to {}", a.join(" ")), true),
            RewriteOutcome::Refused(why) => {
                refused = true;
                (format!("refused: {why}"), false)
            }
            RewriteOutcome::WouldRewrite(_) | RewriteOutcome::Unchanged => continue,
        };
        ctx.log_ledger(
            &requested_by,
            &r.role,
            "synapse-only",
            caller_role.as_deref(),
            None,
            None,
            &outcome,
            acted,
        );
    }
    ctx.out(&render(&rows, dry_run));
    if all {
        ctx.out(&refused_summary(&rows));
    }
    if refused {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `lane-restart specs both-channels --role R... [--dry-run]` - writing is PM only; a dry run writes
/// nothing and any caller may read it. Brings a spec to `--mcp-config <role>.mcp.json` plus
/// `server:claude-peers server:synapse`; one backup, never overwritten.
pub(super) fn both_channels(ctx: &mut Ctx, roles: crate::cli::Roles, dry_run: bool) -> ExitCode {
    use crate::spec_channels::rewrite_roles;
    use crate::spec_synapse::{render, RewriteOutcome};
    let all = roles == crate::cli::Roles::All;
    let roles = resolve(ctx, roles);
    let caller = ctx.verified_caller();
    let caller_role = caller.as_ref().ok().cloned();
    let requested_by = caller_role
        .clone()
        .unwrap_or_else(|| "unverified".to_string());
    if !dry_run && caller_role.as_deref() != Some(start::PM_ROLE) {
        let why = match caller {
            Ok(r) => format!("only the PM session may rewrite specs; this is running under {r:?}"),
            Err(e) => format!(
                "only the PM session may rewrite specs, and the caller could not be verified: {e}"
            ),
        };
        ctx.errln(&format!(
            "lane-restart specs both-channels: refused - {why}"
        ));
        ctx.log_ledger(
            &requested_by,
            &roles.join(","),
            "both-channels",
            caller_role.as_deref(),
            None,
            None,
            &format!("refused: {why}"),
            false,
        );
        return ExitCode::FAILURE;
    }
    let state = ctx.state_dir.clone();
    let rows = rewrite_roles(&state, &roles, dry_run, chrono::Utc::now());
    let mut refused = false;
    for r in &rows {
        let (outcome, acted) = match &r.outcome {
            RewriteOutcome::Rewrote(a) => (format!("rewrote launch_args to {}", a.join(" ")), true),
            RewriteOutcome::Refused(why) => {
                refused = true;
                (format!("refused: {why}"), false)
            }
            RewriteOutcome::WouldRewrite(_) | RewriteOutcome::Unchanged => continue,
        };
        ctx.log_ledger(
            &requested_by,
            &r.role,
            "both-channels",
            caller_role.as_deref(),
            None,
            None,
            &outcome,
            acted,
        );
    }
    ctx.out(&render(&rows, dry_run));
    if all {
        ctx.out(&refused_summary(&rows));
    }
    if refused {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `lane-restart specs set (--role R... | --all) <fields> [--dry-run]` - writing is PM only; a dry
/// run writes nothing and any caller may read it. See `spec_set`.
pub(super) fn set(
    ctx: &mut Ctx,
    roles: crate::cli::Roles,
    opts: crate::spec_set::SetOptions,
    dry_run: bool,
) -> ExitCode {
    use crate::spec_set::{render, set_roles};
    use crate::spec_synapse::RewriteOutcome;
    let all = roles == crate::cli::Roles::All;
    let roles = resolve(ctx, roles);
    let caller = ctx.verified_caller();
    let caller_role = caller.as_ref().ok().cloned();
    let requested_by = caller_role
        .clone()
        .unwrap_or_else(|| "unverified".to_string());
    if !dry_run && caller_role.as_deref() != Some(start::PM_ROLE) {
        let why = match caller {
            Ok(r) => format!("only the PM session may rewrite specs; this is running under {r:?}"),
            Err(e) => format!(
                "only the PM session may rewrite specs, and the caller could not be verified: {e}"
            ),
        };
        ctx.errln(&format!("lane-restart specs set: refused - {why}"));
        ctx.log_ledger(
            &requested_by,
            &roles.join(","),
            "set",
            caller_role.as_deref(),
            None,
            None,
            &format!("refused: {why}"),
            false,
        );
        return ExitCode::FAILURE;
    }
    let state = ctx.state_dir.clone();
    let rows = set_roles(&state, &roles, &opts, dry_run, chrono::Utc::now());
    let mut refused = false;
    for r in &rows {
        let (outcome, acted) = match &r.outcome {
            RewriteOutcome::Rewrote(c) => (format!("set {}", c.join("; ")), true),
            RewriteOutcome::Refused(why) => {
                refused = true;
                (format!("refused: {why}"), false)
            }
            RewriteOutcome::WouldRewrite(_) | RewriteOutcome::Unchanged => continue,
        };
        ctx.log_ledger(
            &requested_by,
            &r.role,
            "set",
            caller_role.as_deref(),
            opts.model.as_deref(),
            None,
            &outcome,
            acted,
        );
    }
    ctx.out(&render(&rows, dry_run));
    if all {
        ctx.out(&refused_summary(&rows));
    }
    if refused {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `lane-restart specs audit [--role R]... [--json]` - any caller; writes nothing, not even a
/// ledger row. Exit 1 when any row has a failing finding.
pub(super) fn audit(ctx: &mut Ctx, roles: Vec<String>, json: bool) -> ExitCode {
    use crate::spec_audit::{audit, common_home, findings, render, Severity};
    let now = chrono::Utc::now();
    let state = ctx.state_dir.clone();
    // The rows come first: they name the identity home the daemon is asked through.
    let rows0 = audit(&state, &roles, &Err(String::new()), now);
    let presence = ctx.presence(common_home(&rows0).as_deref());
    let rows = audit(&state, &roles, &presence, now);
    if json {
        let v: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                let f = findings(r, now);
                serde_json::json!({
                    "row": r,
                    "findings": f.iter().map(|(s, t)| serde_json::json!({"severity": s, "text": t})).collect::<Vec<_>>(),
                })
            })
            .collect();
        ctx.outln(&serde_json::to_string_pretty(&v).unwrap_or_default());
        let failing = rows
            .iter()
            .any(|r| findings(r, now).iter().any(|(s, _)| *s == Severity::Fail));
        return if failing {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        };
    }
    let (text, ok) = render(&rows, &presence, now);
    ctx.out(&text);
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests;
