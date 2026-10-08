// SPDX-License-Identifier: MIT OR Apache-2.0
//! The control tab's text (M4; `DESIGN-REVISION-2.md` §6.3, PM ruling 2026-10-08: text in the
//! terminal, no TUI). A **summary and the exceptions**, never an unbounded list: hundreds of waiting
//! agents stay reviewable. Exceptions come first (refused, widened mode, flagged, new), each capped;
//! then a capped list of who would start. Pure: the caller supplies the plan and, if there is one, the
//! plan of the last approved restore to compare against.

use crate::plan::{Entry, Plan};
use std::collections::HashMap;
use std::fmt::Write;

/// How many lines each section shows before saying how many more there are.
pub const DEFAULT_CAP: usize = 10;

const MODES: &[&str] = &["default", "acceptEdits", "auto", "bypassPermissions"];

/// Wider is higher. No mode means the default; a mode agentlife does not know is ranked above every
/// known one, so it is always shown as widened.
fn rank(mode: Option<&str>) -> usize {
    match mode {
        None => 0,
        Some(m) => MODES.iter().position(|k| *k == m).unwrap_or(MODES.len()),
    }
}

fn name(e: &Entry) -> &str {
    e.name.as_deref().unwrap_or(&e.agent_id)
}

fn section(out: &mut String, title: &str, lines: &[String], cap: usize) {
    if lines.is_empty() {
        return;
    }
    let _ = writeln!(out, "{title} ({}):", lines.len());
    for l in lines.iter().take(cap) {
        let _ = writeln!(out, "  {l}");
    }
    if lines.len() > cap {
        let _ = writeln!(out, "  ... and {} more", lines.len() - cap);
    }
}

/// `plan` is what waits; `previous` is the last approved plan, if any.
pub fn render(plan: &Plan, previous: Option<&Plan>, cap: usize, pending_id: &str) -> String {
    let mut out = String::new();
    let refused: Vec<String> = plan
        .excluded
        .iter()
        .filter(|x| !x.reason.is_routine())
        .map(|x| {
            format!(
                "{}: {:?} {}",
                x.name.as_deref().unwrap_or(&x.agent_id),
                x.reason,
                x.detail
            )
        })
        .collect();
    let _ = writeln!(
        out,
        "restore waiting for you: {} agents to start, {} held, {} refused (plan {})",
        plan.entries.len(),
        plan.held.len(),
        refused.len(),
        &plan.hash[..12.min(plan.hash.len())]
    );
    section(&mut out, "refused by a hard rule", &refused, cap);

    let before: HashMap<&str, &Entry> = previous
        .map(|p| p.entries.iter().map(|e| (e.agent_id.as_str(), e)).collect())
        .unwrap_or_default();
    let widened: Vec<String> = plan
        .entries
        .iter()
        .filter_map(|e| {
            let old = before.get(e.agent_id.as_str())?;
            (rank(e.mode.as_deref()) > rank(old.mode.as_deref())).then(|| {
                format!(
                    "{}: {} -> {}",
                    name(e),
                    old.mode.as_deref().unwrap_or("default"),
                    e.mode.as_deref().unwrap_or("default")
                )
            })
        })
        .collect();
    section(
        &mut out,
        "mode wider than at the last approved restore",
        &widened,
        cap,
    );

    let flagged: Vec<String> = plan
        .entries
        .iter()
        .flat_map(|e| e.flags.iter().map(move |f| format!("{}: {f}", name(e))))
        .collect();
    section(&mut out, "flagged", &flagged, cap);

    match previous {
        Some(_) => {
            let new: Vec<String> = plan
                .entries
                .iter()
                .filter(|e| !before.contains_key(e.agent_id.as_str()))
                .map(|e| name(e).to_string())
                .collect();
            section(&mut out, "new since the last approved restore", &new, cap);
        }
        None => {
            let _ = writeln!(
                out,
                "no earlier approved restore to compare with: new and widened agents cannot be told apart"
            );
        }
    }

    let held: Vec<String> = plan
        .held
        .iter()
        .map(|h| format!("{}: {}", h.name.as_deref().unwrap_or(&h.agent_id), h.why))
        .collect();
    section(&mut out, "held back", &held, cap);

    let who: Vec<String> = plan.entries.iter().map(|e| name(e).to_string()).collect();
    section(&mut out, "would start", &who, cap);
    let _ = writeln!(
        out,
        "detail: agentlife pending show {pending_id}; answer: agentlife pending approve {pending_id}"
    );
    out
}

#[cfg(test)]
mod tests;
