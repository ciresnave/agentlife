// SPDX-License-Identifier: MIT OR Apache-2.0
//! The real consent backend: OverMind's `user-request` store (0.11.1, crates.io) behind [`Consent`].
//!
//! Three rulings shape it (OverMind `origin/main@04aa5d2`, relayed by the PM 2026-10-10):
//!
//! 1. **agentlife is not a registered lane.** Its [`Requester`] is role `agentlife`, empty
//!    `session_id`, its own pid and start time, `managed: false`, so the prompt reads "agentlife
//!    (pid N) - NOT a registered lane". The binding is the plan hash in the subject
//!    ([`restore_plan_subject`](super::restore_plan_subject)): the kind is `Scope::AnyRequester` and
//!    `MaxGrant::OneUse`, so *who* asks is shown to the person but is not what an approval is matched
//!    on.
//! 2. **One store, shared with every other user of `user-request`**: `locate::dir()`,
//!    `locate::head_copy()` and `locate::PROTECTOR` (DPAPI), never a path of our own. The store is
//!    opened **per call** and dropped before the call returns, so its `store.lock` is never held while
//!    a prompt is up or a restore runs. `USER_REQUEST_DIR` / `USER_REQUEST_HEAD` are honoured by the
//!    crate in debug builds only; tests set them.
//! 3. **What does not map is refused, not guessed.** Anything the channel cannot ask or does not
//!    honour (`Outcome::Refused`) is `Unavailable` with its reason, which leaves the request pending.
//!    Every `Err` from the store means nothing was recorded, spent or granted.
//!
//! On a non-Windows host `Dpapi` refuses to protect or unprotect and `HelloConsent` says Hello does
//! not exist there, so every call fails closed and nothing can be approved: the same split
//! `user-request` itself makes.

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use user_request as ur;
use user_request::store::{AuditOnly, Protector, Reservation, Store};

use super::{
    AnswerError, Asking, Consent, Grant, Kind, Outcome, Prompt, Request, Requester, Resolved,
    Voided,
};

/// How long the person has to answer a prompt before it counts as unanswered (and stays pending).
pub const PROMPT_WAIT: Duration = Duration::from_secs(120);

/// Who agentlife is, as the process table shows it: this process, not a registered lane.
pub fn this_process() -> Result<Requester, String> {
    use crate::identity::{ProcessTable, SysinfoTable};
    let pid = std::process::id();
    let me = SysinfoTable
        .identity_of(pid)
        .ok_or_else(|| format!("cannot read this process (pid {pid}) from the process table"))?;
    Ok(Requester {
        role: crate::pending::ROLE.into(),
        session_id: String::new(),
        claude_pid: pid,
        claude_start_secs: me.start_secs,
        managed: false,
    })
}

fn to_ur_kind(k: Kind) -> ur::KindId {
    match k {
        Kind::RestorePlan => ur::KindId::RestorePlan,
    }
}

fn to_ur_requester(r: &Requester) -> ur::Requester {
    ur::Requester {
        role: r.role.clone(),
        session_id: r.session_id.clone(),
        claude_pid: r.claude_pid,
        claude_start_secs: r.claude_start_secs,
        managed: r.managed,
    }
}

fn to_ur_grant(g: &Grant) -> ur::Grant {
    match g {
        Grant::OneUse => ur::Grant::OneUse,
        Grant::For { secs } => ur::Grant::For { secs: *secs },
        Grant::Until(t) => ur::Grant::Until(*t),
        Grant::Forever => ur::Grant::Forever,
    }
}

fn from_ur_grant(g: &ur::Grant) -> Grant {
    match g {
        ur::Grant::OneUse => Grant::OneUse,
        ur::Grant::For { secs } => Grant::For { secs: *secs },
        ur::Grant::Until(t) => Grant::Until(*t),
        ur::Grant::Forever => Grant::Forever,
    }
}

fn from_ur_kind(k: ur::KindId) -> Result<Kind, String> {
    match k {
        ur::KindId::RestorePlan => Ok(Kind::RestorePlan),
        other => Err(format!(
            "the store holds a {other:?} request, which agentlife does not handle"
        )),
    }
}

fn to_ur_request(req: &Request, requester: &Requester) -> ur::Request {
    ur::Request {
        kind: to_ur_kind(req.kind),
        subject: req.subject.clone(),
        summary: req.summary.clone(),
        requester: to_ur_requester(requester),
        reason: req.reason.clone(),
    }
}

/// The shared `user-request` store, opened for each call and dropped at its end.
pub struct UserRequestBackend {
    dir: PathBuf,
    head: PathBuf,
    protector: Box<dyn Protector>,
    requester: Requester,
}

impl UserRequestBackend {
    /// The production store, where every `user-request` user looks for it (`locate`), protected by
    /// DPAPI. Opens nothing yet.
    pub fn production(requester: Requester) -> Result<Self, String> {
        Ok(Self::with_parts(
            ur::locate::dir()?,
            ur::locate::head_copy(),
            Box::new(ur::locate::PROTECTOR),
            requester,
        ))
    }

    /// A store in `dir` with its audit head copy at `head` and `protector` keeping its key. Tests use
    /// this with a protector that works on both CI legs.
    pub fn with_parts(
        dir: PathBuf,
        head: PathBuf,
        protector: Box<dyn Protector>,
        requester: Requester,
    ) -> Self {
        Self {
            dir,
            head,
            protector,
            requester,
        }
    }

    /// For a request: creates the store when there is none yet.
    fn open(&self) -> Result<Store, String> {
        Store::open(&self.dir, self.protector.as_ref(), Some(self.head.clone()))
    }

    /// For a change to something that must already exist: `None` when there is no store.
    fn open_existing(&self) -> Result<Option<Store>, String> {
        Store::open_existing(&self.dir, self.protector.as_ref(), Some(self.head.clone()))
    }

    /// For a read: nothing is written, moved or recorded. `None` when there is no store.
    fn inspect(&self) -> Result<Option<Store>, String> {
        Store::inspect(&self.dir, self.protector.as_ref(), Some(self.head.clone()))
    }
}

/// Why `begin_answer` failed, from what the store looks like afterwards rather than from its prose.
/// A record that is gone was voided and **nobody was asked**; the message only picks which kind of
/// void (the store checks the seal first, then the hash, then the grant).
fn classify_begin_error(
    message: &str,
    record_bound_hash: &str,
    bound_hash_now: &str,
    record_still_there: bool,
    store_trusted: bool,
) -> AnswerError {
    if !record_still_there {
        return AnswerError::Voided(if message.contains("altered") {
            Voided::Altered
        } else if record_bound_hash != bound_hash_now {
            Voided::Stale
        } else {
            Voided::Ended
        });
    }
    if !store_trusted {
        return AnswerError::Unavailable(message.to_string());
    }
    if message.contains("already being answered") {
        return AnswerError::AlreadyAnswering;
    }
    AnswerError::Gate(message.to_string())
}

impl Consent for UserRequestBackend {
    fn submit(&mut self, req: &Request, grant: &Grant, bound_hash: &str) -> Result<String, String> {
        if req.role != self.requester.role {
            return Err(format!(
                "this backend asks as '{}', not '{}'",
                self.requester.role, req.role
            ));
        }
        let mut store = self.open()?;
        store.submit(
            &to_ur_request(req, &self.requester),
            &to_ur_grant(grant),
            bound_hash,
        )
    }

    fn begin_answer(&mut self, id: &str, bound_hash: &str) -> Result<Asking, AnswerError> {
        let mut store = self
            .open_existing()
            .map_err(AnswerError::Unavailable)?
            .ok_or(AnswerError::NoSuchRequest)?;
        let Some(held) = store.pending().iter().find(|p| p.id == id).cloned() else {
            return Err(AnswerError::NoSuchRequest);
        };
        match store.begin_answer(id, bound_hash, &AuditOnly) {
            Ok(a) => Ok(Asking {
                pending_id: a.pending_id,
                request: Request {
                    kind: from_ur_kind(a.request.kind).map_err(AnswerError::Unavailable)?,
                    role: a.request.requester.role,
                    subject: a.request.subject,
                    summary: a.request.summary,
                    reason: a.request.reason,
                },
                grant: from_ur_grant(&a.grant),
                bound_hash: a.bound_hash,
                reservation: a.reservation.attempt_id,
            }),
            Err(why) => {
                let there = store.pending().iter().any(|p| p.id == id);
                Err(classify_begin_error(
                    &why,
                    &held.bound_hash,
                    bound_hash,
                    there,
                    store.trustworthy().is_ok(),
                ))
            }
        }
    }

    fn resolve(&mut self, asking: &Asking, outcome: Outcome) -> Result<Resolved, String> {
        let mut store = self
            .open_existing()?
            .ok_or("the consent store is gone: nothing to resolve")?;
        let held = store
            .pending()
            .iter()
            .find(|p| p.id == asking.pending_id)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "pending request {} is no longer pending (answered or withdrawn)",
                    asking.pending_id
                )
            })?;
        let answer = match outcome {
            // An approval is for exactly what the request asked: its kind and subject, by the
            // requester recorded with it, ending in no time at all because a one-use grant is spent,
            // not timed.
            Outcome::Approved => ur::Outcome::Approved(ur::Approval {
                kind: held.request.kind,
                subject: held.request.subject.clone(),
                requester: held.request.requester.clone(),
                approved_at: Utc::now(),
                expires_at: None,
            }),
            Outcome::Cancelled => ur::Outcome::Denied,
            Outcome::TimedOut => ur::Outcome::TimedOut,
            Outcome::Unavailable => ur::Outcome::Unavailable("the person was not asked".into()),
        };
        let granted = store.resolve(
            &Reservation {
                attempt_id: asking.reservation.clone(),
            },
            &answer,
            &AuditOnly,
        )?;
        match (outcome, granted) {
            (Outcome::Approved, Some(_)) => Ok(Resolved::Granted),
            (Outcome::Approved, None) => {
                Err("the store recorded the answer but granted nothing".into())
            }
            (Outcome::Cancelled, _) => Ok(Resolved::Closed),
            (Outcome::TimedOut | Outcome::Unavailable, _) => Ok(Resolved::StillPending),
        }
    }

    fn approved_at(
        &mut self,
        kind: Kind,
        subject: &str,
        requester: &Requester,
    ) -> Result<DateTime<Utc>, String> {
        let none = || format!("no unspent approval for {kind:?} '{subject}'");
        let store = self.inspect()?.ok_or_else(none)?;
        match store.find(to_ur_kind(kind), subject, &to_ur_requester(requester)) {
            Some(g) => Ok(g.approval.approved_at),
            None => Err(match store.trustworthy() {
                Err(why) => format!("{}: the store cannot be trusted: {why}", none()),
                Ok(()) => none(),
            }),
        }
    }

    fn spend_one_use(
        &mut self,
        kind: Kind,
        subject: &str,
        requester: &Requester,
    ) -> Result<String, String> {
        let mut store = self.open_existing()?.ok_or_else(|| {
            format!("no unspent approval for {kind:?} '{subject}': there is no consent store")
        })?;
        store.spend_one_use(to_ur_kind(kind), subject, &to_ur_requester(requester))
    }

    fn withdraw(&mut self, id: &str) -> Result<bool, String> {
        match self.open_existing()? {
            None => Ok(false),
            Some(mut store) => store.withdraw(id),
        }
    }

    fn pending_ids(&self) -> Result<Vec<String>, String> {
        Ok(match self.inspect()? {
            None => vec![],
            Some(store) => store.pending().iter().map(|p| p.id.clone()).collect(),
        })
    }
}

/// Asks the person through `user-request`'s own channel (Windows Hello). `requester` is who the
/// prompt names: this process.
pub struct HelloPrompt {
    pub requester: Requester,
    pub wait: Duration,
}

impl HelloPrompt {
    pub fn new(requester: Requester) -> Self {
        Self {
            requester,
            wait: PROMPT_WAIT,
        }
    }
}

/// What the channel said, in this crate's terms. `Refused` (over the maximum, malformed, or ended
/// while the person decided) was not an answer: it is `Unavailable`, which leaves the request pending.
pub fn outcome_from_channel(o: &ur::Outcome) -> Outcome {
    match o {
        ur::Outcome::Approved(_) => Outcome::Approved,
        ur::Outcome::Denied => Outcome::Cancelled,
        ur::Outcome::TimedOut => Outcome::TimedOut,
        // `Outcome` is non-exhaustive: anything new is "not answered", never an approval.
        ur::Outcome::Unavailable(_) | ur::Outcome::Refused(_) => Outcome::Unavailable,
        _ => Outcome::Unavailable,
    }
}

/// Debug builds only: a test that runs the real binary sets this so no Windows Hello dialog is ever put
/// on a desktop. It can only make the answer **Unavailable**, never an approval, and a release build
/// does not read it (the same discipline as `USER_REQUEST_DIR`).
pub const NO_HELLO_ENV: &str = "AGENTLIFE_NO_HELLO";

fn hello_disabled_for_tests() -> bool {
    cfg!(debug_assertions) && std::env::var_os(NO_HELLO_ENV).is_some_and(|v| !v.is_empty())
}

impl Prompt for HelloPrompt {
    fn ask(&self, asking: &Asking) -> Outcome {
        use ur::Channel;
        if hello_disabled_for_tests() {
            return Outcome::Unavailable;
        }
        let channel = ur::HelloChannel::new(ur::hello::HelloConsent::default());
        let req = to_ur_request(&asking.request, &self.requester);
        outcome_from_channel(&channel.present(&req, &to_ur_grant(&asking.grant), self.wait))
    }
}

#[cfg(test)]
mod tests;
