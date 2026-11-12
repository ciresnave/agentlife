// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart lane-new <role> <repo> [--model M] [--apply --plan <hash>]` - PM only
//! (docs/LANE-NEW.md). A dry run by default; `--apply` creates the repo, seeds it, writes the lane
//! files, provisions the Synapse identity and starts the lane, stopping at the first step whose
//! read-back fails.
//!
//! Moved out of `app.rs` unchanged apart from reading the caller, the roots, GitHub and the host,
//! and writing its text and ledger row, through the [`Ctx`] (thin-main PR 10).

use super::ctx::Ctx;
use crate::start;
use std::process::ExitCode;

pub(super) fn run(
    ctx: &mut Ctx,
    role: String,
    repo: String,
    model: String,
    apply: bool,
    plan: Option<String>,
    no_start: bool,
) -> ExitCode {
    run_target(ctx, role, repo, None, model, apply, plan, no_start)
}

/// `lane-new-worktree <role> <repo> <worktree-path>`: the same command over a git worktree of an
/// existing checkout instead of a new repository (no GitHub repo, clone or seed).
pub(super) fn run_worktree(
    ctx: &mut Ctx,
    role: String,
    repo: String,
    worktree: String,
    model: String,
    apply: bool,
    plan: Option<String>,
) -> ExitCode {
    run_target(
        ctx,
        role,
        repo,
        Some(std::path::PathBuf::from(worktree)),
        model,
        apply,
        plan,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_target(
    ctx: &mut Ctx,
    role: String,
    repo: String,
    worktree: Option<std::path::PathBuf>,
    model: String,
    apply: bool,
    plan: Option<String>,
    no_start: bool,
) -> ExitCode {
    let cmd = if worktree.is_some() {
        "lane-new-worktree"
    } else {
        "lane-new"
    };
    let caller = ctx.verified_caller();
    let caller_role = caller.as_ref().ok().cloned();
    let requested_by = caller_role
        .clone()
        .unwrap_or_else(|| "unverified".to_string());
    if caller_role.as_deref() != Some(start::PM_ROLE) {
        let why = match caller {
            Ok(r) => format!("only the PM session may create a lane; this is running under {r:?}"),
            Err(e) => format!(
                "only the PM session may create a lane, and the caller could not be verified: {e}"
            ),
        };
        ctx.errln(&format!("lane-restart {cmd}: refused - {why}"));
        ctx.log_ledger(
            &requested_by,
            &role,
            "lane-new",
            caller_role.as_deref(),
            None,
            None,
            &format!("refused: {why}"),
            false,
        );
        return ExitCode::FAILURE;
    }
    let roots = match ctx.lane_new_roots() {
        Ok(r) => r,
        Err(e) => {
            ctx.errln(&format!("lane-restart {cmd}: {e}"));
            return ExitCode::FAILURE;
        }
    };
    // The dry run also reads GitHub (repo visibility, main's head and files), the clone directory and
    // the disk; it creates, clones, provisions and starts nothing. `--apply` does all of it, step by
    // step, each read back (docs/LANE-NEW.md).
    let vcs = ctx.lane_new_vcs();
    let host = ctx.lane_new_host(&roots);
    // `--no-start`: the same host with step 7 skipped (steps 0 to 6; the spec stays suspended).
    let no_start_host = host.as_deref().map(crate::lane_new_host::NoStart);
    let host: Option<&dyn crate::lane_new_host::Host> = if no_start {
        no_start_host
            .as_ref()
            .map(|h| h as &dyn crate::lane_new_host::Host)
    } else {
        host.as_deref()
    };
    // `--apply` goes through `run_apply`, which refuses unless `--plan` is the hash a dry run of exactly
    // these arguments printed: a typo'd role or repo cannot be applied without being planned first.
    let report = if let Some(wt) = worktree.as_deref() {
        if apply {
            crate::lane_new::run_apply_worktree(
                &roots,
                vcs.as_deref(),
                host,
                &role,
                &repo,
                wt,
                &model,
                plan.as_deref(),
                chrono::Utc::now(),
            )
        } else {
            crate::lane_new::run_full_worktree(
                &roots,
                vcs.as_deref(),
                host,
                &role,
                &repo,
                wt,
                &model,
                false,
                chrono::Utc::now(),
            )
        }
    } else if apply {
        crate::lane_new::run_apply(
            &roots,
            vcs.as_deref(),
            host,
            &role,
            &repo,
            &model,
            plan.as_deref(),
            chrono::Utc::now(),
        )
    } else {
        crate::lane_new::run_full(
            &roots,
            vcs.as_deref(),
            host,
            &role,
            &repo,
            &model,
            false,
            chrono::Utc::now(),
        )
    };
    ctx.out(&report.render(apply));
    if apply {
        let steps: Vec<String> = report
            .rows
            .iter()
            .map(|r| format!("{}:{:?}", r.n, r.status))
            .collect();
        ctx.log_ledger(
            &requested_by,
            &role,
            "lane-new",
            caller_role.as_deref(),
            Some(&model),
            Some(&match &worktree {
                Some(wt) => format!("repo {repo} worktree {}", wt.display()),
                None => format!("repo {repo}"),
            }),
            &format!(
                "{}: {}",
                if report.failed() { "failed" } else { "created" },
                steps.join(" ")
            ),
            report
                .rows
                .iter()
                .any(|r| r.status == crate::lane_new::Status::Ok),
        );
    }
    if report.failed() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests;
