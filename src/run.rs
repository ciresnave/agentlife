// SPDX-License-Identifier: MIT OR Apache-2.0
//! Running a restore: ask, spend, execute, report (M4, `docs/CONSENT.md`, `RESTORE-GAP-ANALYSIS` §4 row 4).
//!
//! The order is the whole point and is fixed here, once, for every command that can start agents
//! (`restore`, `pending approve`, `pending --prompt`):
//!
//! 1. **the caller must be a person** (R10): a lane, or a caller that cannot be told from one, is
//!    refused before the store is touched, because starting agents is not a lane's to ask for;
//! 2. one restore at a time (`restore.lock`), held from here to the report;
//! 3. the agent list is **shown**, then the person is asked ([`pending::answer`]);
//! 4. an approval is **spent** ([`pending::spend_approval`]) and only on `Ok`
//! 5. the frozen plan that was approved is executed, and its report written to `<home>/reports`.
//!
//! Everything that touches the world is injected (`consent`, `prompt`, `execute`, `show`), so the tests
//! drive the order with a fake store and a scripted prompt and never reach a desktop.

use crate::caller::Caller;
use crate::clock::Clock;
use crate::consent::{Consent, Prompt, Requester};
use crate::home::Home;
use crate::identity::{ProcessIdentity, ProcessTable};
use crate::pending::{self, Answered};
use crate::plan::{self, Plan};
use crate::restore::{write_report, Report, RestoreLock};
use crate::summary;
use std::path::PathBuf;

/// What came of an attempt that was allowed to ask.
#[derive(Debug)]
pub enum Ran {
    /// The plan was approved, spent and executed. `report_path` is where the report was written, or why
    /// it could not be (the restore has happened either way).
    Executed {
        pending_id: String,
        report: Box<Report>,
        report_path: Result<PathBuf, String>,
    },
    /// The person cancelled: the request is closed and nothing started.
    Cancelled { pending_id: String },
    /// Nobody answered, or the channel could not ask. The request stays pending; nothing started.
    StillPending { pending_id: String },
    /// The person was not asked (cooldown, a prompt already up, an unreachable store). Still pending.
    NotAsked { pending_id: String, why: String },
    /// Closed without asking: the plan is not the one that was asked about. `diffs` names the agents.
    Voided {
        pending_id: String,
        why: String,
        diffs: Vec<String>,
    },
    /// The plan starts nothing, so there is nothing to ask about.
    NothingToStart,
}

/// Everything a run needs from outside.
pub struct Runtime<'a> {
    pub home: &'a Home,
    pub consent: &'a mut dyn Consent,
    pub prompt: &'a dyn Prompt,
    /// Who agentlife asks as and spends as (`consent::real::this_process`).
    pub requester: &'a Requester,
    pub caller: &'a Caller,
    pub clock: &'a dyn Clock,
    pub table: &'a dyn ProcessTable,
    /// This process, for the restore lock.
    pub me: &'a ProcessIdentity,
    /// Receives the text a person reads **before** being asked (the agent list).
    pub show: &'a dyn Fn(&str),
    /// Builds the plan from the registry and the process table **now**. Called again after the person
    /// answered and before anything is spent: the plan that runs is the plan whose hash was approved.
    pub rebuild: &'a dyn Fn() -> Result<Plan, String>,
    /// Starts the approved plan. Called only after the approval was spent.
    pub execute: &'a dyn Fn(&Plan) -> Report,
}

/// Does the entry's launch directory hold a HANDOFF to continue from: `HANDOFF.md`, or
/// `<NAME>-HANDOFF.md` for a plain-identifier name (so the PM's `PM-HANDOFF.md` is found)?
pub fn entry_has_handoff(e: &plan::Entry) -> bool {
    let base = std::path::Path::new(&e.cwd);
    if base.join("HANDOFF.md").is_file() {
        return true;
    }
    e.name.as_deref().is_some_and(|n| {
        let plain = !n.is_empty()
            && n.len() <= 64
            && n.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        plain
            && base
                .join(format!("{}-HANDOFF.md", n.to_ascii_uppercase()))
                .is_file()
    })
}

/// Only a person at a terminal (or the logon task, which has no `claude` above it) may start agents.
pub fn require_person(caller: &Caller) -> Result<(), String> {
    match caller {
        Caller::Person => Ok(()),
        Caller::Agent { .. } => Err(
            "refused: this command was run from inside an agent session. Starting agents is for a person at a terminal or the logon task, never for a lane"
                .into(),
        ),
        Caller::Unclear(why) => Err(format!(
            "refused: a `claude` process is above this one but could not be identified ({why}); treated as an agent, not a person"
        )),
    }
}

fn acquire(rt: &Runtime) -> Result<RestoreLock, String> {
    RestoreLock::acquire(rt.home, rt.me, rt.table).map_err(|e| e.to_string())
}

/// `agentlife restore`: freezes `plan`, records the request, shows the list, asks, spends, executes.
pub fn restore_now(rt: &mut Runtime, plan: &Plan, reason: &str) -> Result<Ran, String> {
    require_person(rt.caller)?;
    if plan.entries.is_empty() {
        return Ok(Ran::NothingToStart);
    }
    let _lock = acquire(rt)?;
    let rec = pending::create(rt.home, rt.consent, plan, reason, rt.clock.now())?;
    run_locked(rt, &rec.pending_id, plan)
}

/// `agentlife pending approve <id>` and `pending --prompt`: asks about an existing request and, on
/// approval, runs it at once (an approval is only good for [`pending::APPROVAL_FRESH_SECS`]).
/// The plan is rebuilt now; a different plan voids the request unasked.
pub fn approve_pending(rt: &mut Runtime, id: &str) -> Result<Ran, String> {
    require_person(rt.caller)?;
    let _lock = acquire(rt)?;
    let rebuilt = (rt.rebuild)()?;
    run_locked(rt, id, &rebuilt)
}

fn run_locked(rt: &mut Runtime, id: &str, rebuilt: &Plan) -> Result<Ran, String> {
    let rec = pending::load(rt.home, id)?;
    // The list comes first, from the frozen plan the person will actually be approving. If it cannot
    // be read, `answer` voids the request below; nothing is asked on a plan that cannot be shown.
    if let Ok(f) = plan::load_frozen(&pending::frozen_path(rt.home, &rec)) {
        let previous = pending::last_approved_plan(rt.home);
        (rt.show)(&summary::render(
            &f.plan,
            previous.as_ref(),
            summary::DEFAULT_CAP,
            id,
        ));
    }
    let pending_id = id.to_string();
    match pending::answer(rt.home, rt.consent, rt.prompt, id, rebuilt, rt.clock.now())? {
        Answered::Approved(approved) => {
            // The person may have taken a while: recompute before spending. If the plan is no longer
            // the one whose hash was approved, nothing is spent and nothing starts (the approval stays
            // unspent and goes stale; ask again).
            let now_plan = (rt.rebuild)().map_err(|e| {
                format!("not running {id}: cannot rebuild the plan to check it ({e}); nothing was spent, nothing was started")
            })?;
            if now_plan.hash != approved.plan.hash {
                let diffs = plan::check_unchanged(&approved.plan, &now_plan)
                    .err()
                    .unwrap_or_default();
                return Err(format!(
                    "not running {id}: the plan changed while the person decided; nothing was spent, nothing was started. Differences: {}",
                    if diffs.is_empty() {
                        "(same agents, different parameters)".to_string()
                    } else {
                        diffs.join("; ")
                    }
                ));
            }
            // Spend first; run only on `Ok`. The plan that runs is the one the spend returned.
            let frozen =
                pending::spend_approval(rt.home, rt.consent, rt.requester, id, rt.clock.now())?;
            let report = (rt.execute)(&frozen.plan);
            let report_path = write_report(rt.home, &report).map_err(|e| e.to_string());
            Ok(Ran::Executed {
                pending_id,
                report: Box::new(report),
                report_path,
            })
        }
        Answered::Cancelled => Ok(Ran::Cancelled { pending_id }),
        Answered::StillPending => Ok(Ran::StillPending { pending_id }),
        Answered::NotAsked(why) => Ok(Ran::NotAsked { pending_id, why }),
        Answered::Voided { why, diffs } => Ok(Ran::Voided {
            pending_id,
            why,
            diffs,
        }),
    }
}

#[cfg(test)]
mod tests;
