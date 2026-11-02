// SPDX-License-Identifier: MIT OR Apache-2.0

//! What `start`, `stop` and the restart command share, through the [`Ctx`]: the disk floor, the
//! leftover-tab close and the awaiting-dialog notice. They were free functions of `app.rs` (and,
//! while the restart command had not moved, copies in `start_cmd` and `stop_cmd`); thin-main PR 13
//! gives them one home.

use super::ctx::Ctx;
use crate::tab_close;

/// The disk floor (`disk.rs`), checked before any launch. A refusal is printed as `kind: refused -
/// ...` and the caller exits 1 and logs it; an unreadable probe is a warning, not a refusal.
pub(super) fn disk_gate(ctx: &mut Ctx, kind: &str) -> Result<(), crate::disk::Refusal> {
    match crate::disk::guard(&ctx.state_dir) {
        Ok(crate::disk::GuardOk::Free(_)) => Ok(()),
        Ok(crate::disk::GuardOk::Unknown(why)) => {
            ctx.errln(&format!(
                "lane-restart {kind}: warning - free disk space was not checked: {why}"
            ));
            Ok(())
        }
        Err(refusal) => {
            ctx.errln(&format!("lane-restart {kind}: refused - {refusal}"));
            Err(refusal)
        }
    }
}

/// RESTART-TOOL-DESIGN.md §5: closes the tab a hand-started lane's shell keeps open once its
/// claude is killed (`tab_close.rs`). Only after a relaunch that acted - after a failed one the
/// tab stays, so a human has a prompt to relaunch from. Every outcome is logged, and a closed
/// shell is logged BEFORE the kill, in case closing the tab takes this process with it.
#[allow(clippy::too_many_arguments)]
pub(super) fn close_leftover_tab(
    ctx: &mut Ctx,
    facts: &dyn crate::facts::SystemFacts,
    kind: &str,
    requested_by: &str,
    role: &str,
    leftover_shell: Result<tab_close::ShellCandidate, tab_close::LeftAlone>,
    relaunch_acted: bool,
    killed_pid: u32,
    killed_identity: &crate::facts::ProcessIdentity,
) {
    // A ledger line of `kind`, with `caller_role` set whenever `requested_by` is a verified role
    // (that is, neither the `--self` label nor "unverified").
    let caller_role =
        (requested_by != "self" && requested_by != "unverified").then_some(requested_by);
    let result = leftover_shell.and_then(|shell| {
        if !relaunch_acted {
            return Err(tab_close::LeftAlone::RelaunchFailed);
        }
        tab_close::close(facts, &shell, killed_pid, killed_identity, &mut |shell| {
            ctx.log_ledger(
                requested_by,
                role,
                kind,
                caller_role,
                None,
                None,
                &format!("closing the leftover tab: terminating shell {shell}"),
                true,
            );
        })
    });
    if let Err(why) = result {
        ctx.log_ledger(
            requested_by,
            role,
            kind,
            caller_role,
            None,
            None,
            &format!("leftover tab's shell left alone: {why}"),
            false,
        );
    }
}

/// A machine-readable status line - PM finding, 2026-09-18 (fifth real-restart retest): so the PM
/// lane (or anything else watching this process's stdout) can act on `awaiting_confirmation`
/// promptly, e.g. by sending CireSnave a phone notification that a relaunched lane needs a human to
/// confirm the dev-channels dialog. Printed the moment it is first detected, never only at the end
/// of the (up to ~15 minute) wait.
pub(super) fn print_status_json(ctx: &mut Ctx, outcome: &str, role: &str) {
    let line = serde_json::json!({
        "event": "lane-restart-status",
        "role": role,
        "outcome": outcome,
        "at": chrono::Utc::now().to_rfc3339(),
    });
    ctx.outln(&line.to_string());
}
