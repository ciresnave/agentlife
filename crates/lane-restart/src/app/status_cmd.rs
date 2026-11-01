// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart status [--json] [--quiet-over <secs>] [--peers]` - RESTART-TOOL-DESIGN.md 14.2 /
//! 14.10. Read-only: for every role with a lane state or a launch spec, whether it is running,
//! suspended, crashed or never run, and whether it can be started again from its spec. Liveness is
//! the process table's (pid and start time), never a file's age.
//!
//! Moved out of `app.rs` unchanged apart from reading its inputs (the process facts, the notify
//! transport, the broker's peers, the process-parent walk) and writing its text through the
//! [`Ctx`] (thin-main PR 4).

use super::ctx::Ctx;
use crate::spec;
use std::process::ExitCode;

/// ` QUIET>Ns` after the quiet seconds when `--quiet-over N` was given and the role is over it.
fn quiet_mark(a: &crate::spec::Activity, quiet_over: Option<i64>) -> String {
    match (a.quiet_over, quiet_over) {
        (Some(true), Some(n)) => format!(" QUIET>{n}s"),
        _ => String::new(),
    }
}

/// `\tduplicate-risk: host pid N, claude pid M`, or nothing for a normal row.
fn duplicate_text(d: &[crate::duplicate::Duplicate]) -> String {
    if d.is_empty() {
        return String::new();
    }
    let list: Vec<String> = d.iter().map(ToString::to_string).collect();
    format!("\tduplicate-risk: {}", list.join(", "))
}

fn peers_text(p: &crate::peers_join::RolePeers) -> String {
    use crate::peers_join::RolePeers;
    match p {
        RolePeers::Unknown(u) => u.clone(),
        RolePeers::Peers(v) if v.is_empty() => "-".to_string(),
        RolePeers::Peers(v) => v
            .iter()
            .map(|p| match &p.summary {
                Some(s) => format!("{} {s:?}", p.id),
                None => p.id.clone(),
            })
            .collect::<Vec<_>>()
            .join("; "),
    }
}

/// The `channels=` column of a live lane: `mcp+peers+synapse`, each missing flag shown as `-` and a
/// lane whose state recorded no launch arguments as `?`. Not live: no column.
fn channels_text(r: &crate::spec::RoleStatus) -> String {
    match (&r.activity, &r.channels) {
        (Some(_), Some(c)) => format!(
            "\tchannels={}{}",
            c.text(),
            if c.reaches_synapse() {
                ""
            } else {
                " NO-SYNAPSE"
            }
        ),
        (Some(_), None) => "\tchannels=?".to_string(),
        (None, _) => String::new(),
    }
}

pub(super) fn run(
    ctx: &mut Ctx,
    json: bool,
    quiet_over: Option<i64>,
    with_peers: bool,
) -> ExitCode {
    let state = ctx.state_dir.clone();
    let facts = ctx.facts();
    let mut report = spec::status_all(&state, &*facts);
    if let Some(secs) = quiet_over {
        report.mark_quiet_over(secs);
    }
    crate::duplicate::annotate(&mut report, &state, &*facts);
    let peers_moved = with_peers && ctx.transport() == crate::notify::Transport::Synapse;
    if peers_moved {
        // The published lane-restart crate does not depend on Synapse: the join moved.
        ctx.errln(
            "lane-restart status --peers: moved (notify_transport is synapse): run `agentlife status --peers`",
        );
    }
    if with_peers && !peers_moved {
        let parents = ctx.parents();
        let broker = ctx.broker_peers();
        crate::peers_join::annotate(
            &mut report,
            &state,
            &*facts,
            &|pid| parents.parent_of(pid).map(|p| p.0),
            broker,
        );
    }
    if json {
        // The JSON shape is unchanged; the disk line goes to stderr so a reader of stdout is not
        // broken by it.
        ctx.errln(&crate::disk::status_line(&state));
        match serde_json::to_string_pretty(&report) {
            Ok(s) => ctx.outln(&s),
            Err(e) => {
                ctx.errln(&format!("lane-restart status: {e}"));
                return ExitCode::FAILURE;
            }
        }
        return ExitCode::SUCCESS;
    }
    ctx.outln(&crate::disk::status_line(&state));
    for r in &report.roles {
        ctx.outln(&format!(
            "{}\t{:?}\tspec={:?}\tstartable={}\t{}{}{}",
            r.role,
            r.state,
            r.spec,
            r.startable,
            match &r.activity {
                Some(a) if a.idle => format!(
                    "idle=true quiet={}s{}",
                    a.quiet_secs,
                    quiet_mark(a, quiet_over)
                ),
                Some(a) => format!(
                    "idle=false({}) quiet={}s{}",
                    a.idle_blockers.join("+"),
                    a.quiet_secs,
                    quiet_mark(a, quiet_over)
                ),
                None => "idle=-".to_string(),
            },
            match &r.handoff {
                Some(h) => format!(
                    "\thandoff={}B/{}B{}",
                    h.bytes,
                    h.ceiling,
                    if h.over { " OVER" } else { "" }
                ),
                None => "\thandoff=none".to_string(),
            } + &r
                .nested_in
                .as_deref()
                .map(|p| format!("\tnested-in={p}"))
                .unwrap_or_default()
                + &channels_text(r),
            r.detail
                .as_deref()
                .map(|d| format!("\t{d}"))
                .unwrap_or_default()
                + &duplicate_text(&r.duplicate_risk)
        ));
        if let Some(p) = &r.peers {
            ctx.outln(&format!("  peers: {}", peers_text(p)));
        }
    }
    if let Some(e) = &report.duplicate_error {
        ctx.outln(&format!(
            "(duplicate-risk: the process table could not be read, so no role was checked: {e})"
        ));
    }
    if let Some(e) = &report.peers_error {
        ctx.outln(&format!(
            "(peers: the broker could not be read, so every role is unknown: {e})"
        ));
    }
    if let Some(un) = &report.peers_unmatched {
        for u in un {
            ctx.outln(&format!(
                "unmatched peer {} in {}: no role's live claude is its parent{}",
                u.id,
                u.cwd,
                u.summary
                    .as_deref()
                    .map(|s| format!(" - {s:?}"))
                    .unwrap_or_default()
            ));
        }
    }
    let cov = &report.coverage;
    ctx.outln(&format!(
        "launch specs: {} of {} role(s) have a complete one{}",
        cov.complete,
        cov.total,
        if cov.gaps.is_empty() {
            ""
        } else {
            "; without:"
        }
    ));
    for g in &cov.gaps {
        ctx.outln(&format!(
            "  {} role(s): {}: {}",
            g.roles.len(),
            g.why,
            g.roles.join(", ")
        ));
    }
    if !report.skipped.is_empty() {
        ctx.outln(&format!(
            "({} other file(s) in the state directory are neither a lane state nor a launch spec: {})",
            report.skipped.len(),
            report.skipped.join(", ")
        ));
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests;
