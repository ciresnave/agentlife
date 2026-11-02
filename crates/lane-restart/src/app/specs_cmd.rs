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

/// `lane-restart specs synapse-only --role R... [--strict] [--dry-run]` - PM only. Rewrites a spec's
/// `launch_args` to the Synapse-only form (docs/SYNAPSE-ONLY-CUTOVER.md); one backup, never overwritten.
pub(super) fn synapse_only(
    ctx: &mut Ctx,
    roles: Vec<String>,
    strict: bool,
    dry_run: bool,
) -> ExitCode {
    use crate::spec_synapse::{render, rewrite_roles, RewriteOutcome};
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
    if refused {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `lane-restart specs both-channels --role R... [--dry-run]` - writing is PM only; a dry run writes
/// nothing and any caller may read it. Brings a spec to `--mcp-config <role>.mcp.json` plus
/// `server:claude-peers server:synapse`; one backup, never overwritten.
pub(super) fn both_channels(ctx: &mut Ctx, roles: Vec<String>, dry_run: bool) -> ExitCode {
    use crate::spec_channels::rewrite_roles;
    use crate::spec_synapse::{render, RewriteOutcome};
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
    if refused {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests;
