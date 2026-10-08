// SPDX-License-Identifier: MIT OR Apache-2.0
//! The durable pending restore (M4; `DESIGN-REVISION-2.md` §6, `docs/CONSENT.md`).
//!
//! A restore that needs a person's consent does not wait in a process. [`create`] **freezes** the plan
//! (`plan::freeze`), records a consent request bound to the plan's hash, writes
//! `<home>/pending/<id>.json` and returns. Days later [`answer`] rebuilds nothing itself: the caller
//! passes the plan it has rebuilt from the registry **now**, and the consent store compares hashes. A
//! difference voids the request and the person is never asked; the caller then makes a new one from
//! the rebuilt plan (staleness is "the agents are not the same agents", never a clock).
//!
//! The record here says **what is owed and why**; the consent store holds the request itself. A record
//! is never deleted: closing marks it, so the audit trail stays.
//!
//! **Nothing in the binary can reach [`create`] or an approval yet.** The only backend is
//! [`consent::NoBackend`] until OverMind's `user-request` is published, so `agentlife restore` without
//! `--dry-run` still refuses. Tests drive these functions with [`consent::fake::FakeConsent`].

use crate::atomic::{is_temp_name, write_atomic};
use crate::consent::{self, AnswerError, Consent, Grant, Kind, Prompt, Request, Resolved, Voided};
use crate::home::Home;
use crate::lock::{FileLock, DEFAULT_ACQUIRE_TIMEOUT, DEFAULT_STALE_AFTER};
use crate::plan::{self, Frozen, Plan};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const PENDING_SCHEMA: u32 = 1;

/// The role agentlife asks as.
pub const ROLE: &str = "agentlife";

/// PROVISIONAL, pending the ruling on the `RestorePlan` kind (`docs/CONSENT.md`, "Proposal"): the
/// approval is used once, at once, by the process that asked, so the window only has to cover one
/// run. Thirty minutes is the proposed cap; it is not a standing grant.
pub fn plan_consent_grant() -> Grant {
    Grant::For { secs: 30 * 60 }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Closure {
    /// Approved; the plan may be executed.
    Approved,
    /// The person cancelled: a refusal. Closed, not pending (CireSnave: "cancel refuses").
    Cancelled,
    /// `agentlife pending discard`.
    Discarded,
    /// Voided by the consent store, or the frozen plan was unusable. `detail` says why.
    Voided,
    /// A newer pending restore for a different plan replaced it (`DESIGN-REVISION-2.md` §6.5). Kept
    /// for audit; nothing is lost because candidates are always derived from the registry.
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Closed {
    pub at: DateTime<Utc>,
    pub how: Closure,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLine {
    pub agent_id: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingRestore {
    pub schema: u32,
    /// The consent store's id for the request.
    pub pending_id: String,
    pub plan_hash: String,
    /// The frozen plan's file name inside `<home>/plans`.
    pub plan_file: String,
    pub created_at: DateTime<Utc>,
    pub reason: String,
    pub agents: Vec<AgentLine>,
    /// `None` while the request is open.
    pub closed: Option<Closed>,
}

impl PendingRestore {
    pub fn is_open(&self) -> bool {
        self.closed.is_none()
    }

    pub fn render_text(&self) -> String {
        let mut out = format!(
            "{} {} plan {} ({} agents), asked {}: {}\n",
            self.pending_id,
            match &self.closed {
                None => "OPEN".to_string(),
                Some(c) => format!("closed ({:?})", c.how).to_lowercase(),
            },
            &self.plan_hash[..12.min(self.plan_hash.len())],
            self.agents.len(),
            self.created_at.format("%Y-%m-%d %H:%M:%SZ"),
            self.reason
        );
        if let Some(c) = &self.closed {
            out.push_str(&format!(
                "  closed {}: {}\n",
                c.at.format("%Y-%m-%d %H:%M:%SZ"),
                c.detail
            ));
        }
        for a in &self.agents {
            out.push_str(&format!("  {}\n", a.name.as_deref().unwrap_or(&a.agent_id)));
        }
        out
    }
}

fn record_path(home: &Home, id: &str) -> PathBuf {
    home.pending_dir().join(format!("{id}.json"))
}

/// An id becomes a file name, so it must not be able to leave the directory.
fn check_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("{id:?} is not a pending request id"));
    }
    Ok(())
}

fn save(home: &Home, rec: &PendingRestore) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(rec).map_err(|e| e.to_string())?;
    write_atomic(&record_path(home, &rec.pending_id), &body)
        .map_err(|e| format!("cannot write the pending record: {e}"))
}

pub fn load(home: &Home, id: &str) -> Result<PendingRestore, String> {
    check_id(id)?;
    let path = record_path(home, id);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("no pending restore {id} ({}: {e})", path.display()))?;
    let rec: PendingRestore =
        serde_json::from_str(&text).map_err(|e| format!("{} is not valid: {e}", path.display()))?;
    if rec.schema != PENDING_SCHEMA {
        return Err(format!(
            "{} has schema {}, expected {PENDING_SCHEMA}",
            path.display(),
            rec.schema
        ));
    }
    Ok(rec)
}

/// Every record, oldest first, with the files that could not be read.
pub fn list(home: &Home) -> (Vec<PendingRestore>, Vec<(PathBuf, String)>) {
    let mut out = Vec::new();
    let mut problems = Vec::new();
    let Ok(rd) = std::fs::read_dir(home.pending_dir()) else {
        return (out, problems);
    };
    for e in rd.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if is_temp_name(&name) || path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let stem = name.trim_end_matches(".json");
        match load(home, stem) {
            Ok(r) => out.push(r),
            Err(why) => problems.push((path, why)),
        }
    }
    out.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then(a.pending_id.cmp(&b.pending_id))
    });
    (out, problems)
}

fn close(
    home: &Home,
    rec: &mut PendingRestore,
    how: Closure,
    detail: String,
    now: DateTime<Utc>,
) -> Result<(), String> {
    rec.closed = Some(Closed {
        at: now,
        how,
        detail,
    });
    save(home, rec)
}

/// Freezes `plan`, records a consent request bound to its hash, and writes the pending record.
/// Asking the same plan again returns the record it already has. Nothing is shown to anyone.
pub fn create(
    home: &Home,
    consent: &mut dyn Consent,
    plan: &Plan,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<PendingRestore, String> {
    if plan.entries.is_empty() {
        return Err("the plan starts nothing; there is nothing to ask about".into());
    }
    if !plan.hash_is_valid() {
        return Err("the plan's hash does not match its content; refusing to ask about it".into());
    }
    // One creator at a time: otherwise two processes that each saved a record before either scanned
    // would supersede each other and leave nothing open.
    let _lock = FileLock::acquire(
        home.pending_dir().join("create.lock"),
        DEFAULT_ACQUIRE_TIMEOUT,
        DEFAULT_STALE_AFTER,
    )
    .map_err(|e| format!("cannot serialise pending creation: {e}"))?;
    let req = Request {
        kind: Kind::RestorePlan,
        role: ROLE.into(),
        subject: format!("restore {} agents", plan.entries.len()),
        summary: format!(
            "start {} agents (plan {})",
            plan.entries.len(),
            &plan.hash[..12.min(plan.hash.len())]
        ),
        reason: reason.into(),
    };
    let id = consent.submit(&req, &plan_consent_grant(), &plan.hash)?;
    if let Ok(existing) = load(home, &id) {
        if existing.is_open() && existing.plan_hash == plan.hash {
            return Ok(existing);
        }
    }
    let frozen_path = plan::freeze(home, plan, now).map_err(|e| {
        let _ = consent.withdraw(&id);
        format!("cannot freeze the plan: {e}")
    })?;
    let rec = PendingRestore {
        schema: PENDING_SCHEMA,
        pending_id: id.clone(),
        plan_hash: plan.hash.clone(),
        plan_file: frozen_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        created_at: now,
        reason: reason.into(),
        agents: plan
            .entries
            .iter()
            .map(|e| AgentLine {
                agent_id: e.agent_id.clone(),
                name: e.name.clone(),
            })
            .collect(),
        closed: None,
    };
    if let Err(e) = save(home, &rec) {
        // not recorded here, so not kept there
        let _ = consent.withdraw(&id);
        return Err(e);
    }
    supersede_older(home, consent, &rec, now)?;
    Ok(rec)
}

/// A newer boot's pending restore supersedes every older open one for a different plan. Each is
/// withdrawn first, so a prompt already up cannot approve it afterwards, and closed only once that
/// has happened. A request the store no longer holds (`Ok(false)`: already answered, or never
/// there) is left alone, not claimed as superseded: the audit record must not contradict an answer.
/// A directory that cannot be read, a record that cannot be closed or a withdraw that fails is an
/// error, so `create` does not report success while an older request may still be approvable.
fn supersede_older(
    home: &Home,
    consent: &mut dyn Consent,
    newer: &PendingRestore,
    now: DateTime<Utc>,
) -> Result<(), String> {
    let (records, problems) = list(home);
    if let Some((path, why)) = problems.first() {
        return Err(format!(
            "cannot tell which pending restores to supersede: {}: {why}",
            path.display()
        ));
    }
    for mut old in records {
        if !old.is_open() || old.pending_id == newer.pending_id || old.plan_hash == newer.plan_hash
        {
            continue;
        }
        let withdrawn = consent
            .withdraw(&old.pending_id)
            .map_err(|e| format!("cannot withdraw {}: {e}", old.pending_id))?;
        if !withdrawn {
            continue;
        }
        let detail = format!("superseded by {}", newer.pending_id);
        close(home, &mut old, Closure::Superseded, detail, now)?;
    }
    Ok(())
}

/// What came of asking.
#[derive(Debug, Clone, PartialEq)]
pub enum Answered {
    /// Approved. This is the frozen plan the person approved (its hash is the one now held).
    Approved(Box<Frozen>),
    /// The person cancelled: the request is closed.
    Cancelled,
    /// Nobody answered, or the channel could not ask. Still pending.
    StillPending,
    /// The person was **not** asked and the request is still pending (cooldown, a prompt already
    /// up, an unreachable store).
    NotAsked(String),
    /// Closed without asking: the plan changed, a record was altered, or the frozen plan is
    /// unusable. `diffs` names the agents that differ, when the plan changed.
    Voided { why: String, diffs: Vec<String> },
}

/// Asks the person about pending request `id`. `rebuilt` is the plan built from the registry now;
/// its hash is what the consent store compares. A restored request re-prompts: nothing here approves
/// anything by itself.
pub fn answer(
    home: &Home,
    consent: &mut dyn Consent,
    prompt: &dyn Prompt,
    id: &str,
    rebuilt: &Plan,
    now: DateTime<Utc>,
) -> Result<Answered, String> {
    let mut rec = load(home, id)?;
    if let Some(c) = &rec.closed {
        return Err(format!(
            "pending restore {id} is already closed ({:?}): {}",
            c.how, c.detail
        ));
    }
    let frozen = match plan::load_frozen(&home.plans_dir().join(&rec.plan_file)) {
        Ok(f) if f.plan.hash == rec.plan_hash => f,
        Ok(f) => {
            let why = format!(
                "the frozen plan hashes to {} but the request was made for {}",
                f.plan.hash, rec.plan_hash
            );
            let _ = consent.withdraw(id);
            close(home, &mut rec, Closure::Voided, why.clone(), now)?;
            return Ok(Answered::Voided { why, diffs: vec![] });
        }
        Err(e) => {
            let why = e.to_string();
            let _ = consent.withdraw(id);
            close(home, &mut rec, Closure::Voided, why.clone(), now)?;
            return Ok(Answered::Voided { why, diffs: vec![] });
        }
    };
    match consent::answer(consent, prompt, id, &rebuilt.hash) {
        Ok(Resolved::Granted) => {
            // A newer request may have replaced this one while the prompt was up.
            if let Ok(now_rec) = load(home, id) {
                if let Some(c) = &now_rec.closed {
                    if c.how == Closure::Superseded {
                        return Ok(Answered::Voided {
                            why: format!("superseded while the prompt was up: {}", c.detail),
                            diffs: vec![],
                        });
                    }
                }
            }
            // The consent is spent whether or not this write lands, and cannot be spent twice.
            let _ = close(home, &mut rec, Closure::Approved, "approved".into(), now);
            Ok(Answered::Approved(Box::new(frozen)))
        }
        Ok(Resolved::Closed) => {
            close(
                home,
                &mut rec,
                Closure::Cancelled,
                "the person cancelled; a cancel refuses".into(),
                now,
            )?;
            Ok(Answered::Cancelled)
        }
        Ok(Resolved::StillPending) => Ok(Answered::StillPending),
        Err(AnswerError::Voided(v)) => {
            let why = AnswerError::Voided(v.clone()).to_string();
            let diffs = match v {
                Voided::Stale => plan::check_unchanged(&frozen.plan, rebuilt)
                    .err()
                    .unwrap_or_default(),
                _ => vec![],
            };
            close(home, &mut rec, Closure::Voided, why.clone(), now)?;
            Ok(Answered::Voided { why, diffs })
        }
        Err(AnswerError::NoSuchRequest) => {
            let why = "the consent store no longer has this request".to_string();
            close(home, &mut rec, Closure::Voided, why.clone(), now)?;
            Ok(Answered::Voided { why, diffs: vec![] })
        }
        Err(
            e
            @ (AnswerError::AlreadyAnswering | AnswerError::Gate(_) | AnswerError::Unavailable(_)),
        ) => Ok(Answered::NotAsked(e.to_string())),
    }
}

/// `agentlife pending discard`: the requester gives up. The request is withdrawn first, so a prompt
/// already up cannot be approved afterwards; the record is closed only once that has happened.
pub fn discard(
    home: &Home,
    consent: &mut dyn Consent,
    id: &str,
    now: DateTime<Utc>,
) -> Result<(), String> {
    let mut rec = load(home, id)?;
    if let Some(c) = &rec.closed {
        return Err(format!(
            "pending restore {id} is already closed ({:?})",
            c.how
        ));
    }
    consent.withdraw(id)?;
    close(
        home,
        &mut rec,
        Closure::Discarded,
        "discarded by a person".into(),
        now,
    )
}

/// The plan of the most recently approved restore, the baseline the control tab compares against.
/// `None` if nothing was ever approved or its frozen plan cannot be read (then the summary says so).
pub fn last_approved_plan(home: &Home) -> Option<Plan> {
    let (records, problems) = list(home);
    // An unreadable record might be the newest approval: better no baseline than an older one.
    if !problems.is_empty() {
        return None;
    }
    // Newest by close time; a tie (or a clock that moved backwards) is broken by id, deterministically.
    let rec = records
        .iter()
        .filter(|r| {
            r.closed
                .as_ref()
                .is_some_and(|c| c.how == Closure::Approved)
        })
        .max_by_key(|r| (r.closed.as_ref().map(|c| c.at), r.pending_id.clone()))?;
    plan::load_frozen(&frozen_path(home, rec))
        .ok()
        .map(|f| f.plan)
}

/// Where the frozen plan of a record lives.
pub fn frozen_path(home: &Home, rec: &PendingRestore) -> PathBuf {
    home.plans_dir().join(&rec.plan_file)
}

#[cfg(test)]
mod tests;
