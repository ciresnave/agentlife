// SPDX-License-Identifier: MIT OR Apache-2.0
//! Consent (M4): the shape of OverMind's `user-request` durable-pending API, as a trait.
//!
//! **Why a trait.** `user-request` was not on crates.io when this was built (2026-10-08), and
//! CireSnave's Sources rule forbids a path or `git =` dependency (`CLAUDE.md` §9), so the pending
//! restore was built against a trait shaped like the crate's API and a fake that enforces its
//! semantics. It is published now (0.11.1) and [`real::UserRequestBackend`] is the backend
//! [`installed`] returns; the trait stays so the rest of agentlife is tested without a Windows Hello
//! store. Nothing from OverMind is copied here.
//!
//! The semantics every implementation must have (the fake in [`fake`] enforces them):
//!
//! * `submit` records a request that outlives the process and holds **no privilege**. The same
//!   request with the same `bound_hash` returns the id it already has.
//! * `begin_answer(id, bound_hash)` is given the hash the caller holds **now**. A different hash, an
//!   altered record or an ended grant **voids** the request and the person is never asked.
//! * A restored request **re-prompts**; nothing approves by itself. Only the channel's approval
//!   resolves it as granted.
//! * One prompt at a time per request.
//! * **Cancel closes the request** (CireSnave: "cancel refuses"). Only `TimedOut` and `Unavailable`
//!   leave it pending, and `TimedOut` starts a cooldown.
//! * `withdraw` ends a request; a prompt already up can no longer be approved.
//! * `spend_one_use(kind, subject, requester)` is the crate's shape (`Store::spend_one_use`): it
//!   returns the id it spent, or `Err` when **nothing** was spent, and the action must then not run.
//!   The approval is found by what it covers (kind, subject), never by a request id. For
//!   [`Kind::RestorePlan`] the requester does **not** narrow it (`Scope::AnyRequester`): the plan hash
//!   in the subject is the binding, because a restore after a reboot is asked for by a new process.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub mod real;

/// What is asked. `user-request`'s `KindId` is a closed set compiled into that crate; agentlife needs
/// a plan-consent kind added there (an open question for OverMind, recorded in `docs/CONSENT.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// "Start these agents now", bound to a frozen plan's hash.
    RestorePlan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub kind: Kind,
    /// Who asks, by role. The caller supplies it (see `Requester`); `user-request` 0.11.1 has no
    /// process-table constructor.
    pub role: String,
    pub subject: String,
    pub summary: String,
    pub reason: String,
}

/// Who is asking, as the process table shows it. The caller supplies it: `user-request` 0.11.1 has no
/// constructor from the process table. agentlife is not a registered lane, so `managed` is `false`,
/// `session_id` is empty and the prompt says "NOT a registered lane".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requester {
    pub role: String,
    pub session_id: String,
    pub claude_pid: u32,
    pub claude_start_secs: u64,
    pub managed: bool,
}

const PLAN_SUBJECT_PREFIX: &str = "plan ";
const PLAN_HASH_LEN: usize = 64;

/// The only subject a [`Kind::RestorePlan`] request may carry: `plan <hash>`, the plan's SHA-256 in
/// lowercase hex. The real crate refuses anything else, so the fake does too.
pub fn restore_plan_subject(hash: &str) -> Result<String, String> {
    if hash.len() != PLAN_HASH_LEN || !hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(format!(
            "a plan hash is {PLAN_HASH_LEN} lowercase hex digits; got {} characters",
            hash.chars().count()
        ));
    }
    Ok(format!("{PLAN_SUBJECT_PREFIX}{hash}"))
}

/// The hash inside a `plan <hash>` subject, or why the subject is not one.
pub fn parse_restore_plan_subject(subject: &str) -> Result<String, String> {
    let hash = subject
        .strip_prefix(PLAN_SUBJECT_PREFIX)
        .ok_or_else(|| "a restore-plan subject is `plan <hash>`".to_string())?;
    restore_plan_subject(hash).map(|_| hash.to_string())
}

/// How long an approval lasts. Plan consent is [`Grant::OneUse`] (CireSnave: "One-shot."): an approval
/// the requester spends with `spend_one_use` before it acts. No duration, never `Forever`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Grant {
    /// Spent once, by the requester, before it acts.
    OneUse,
    For {
        secs: i64,
    },
    Until(DateTime<Utc>),
    Forever,
}

/// A prompt the caller must now show and then resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asking {
    pub pending_id: String,
    pub request: Request,
    pub grant: Grant,
    pub bound_hash: String,
    pub reservation: String,
}

/// How the person's answer came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Approved,
    /// A denial: the request is closed.
    Cancelled,
    /// Nobody answered: the request stays pending.
    TimedOut,
    /// The channel could not ask: the request stays pending.
    Unavailable,
}

/// What `resolve` did to the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolved {
    /// Approved: the request is spent.
    Granted,
    /// Cancelled: the request is closed, nothing granted.
    Closed,
    /// Not answered: the request is still pending.
    StillPending,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Voided {
    /// The `bound_hash` held now is not the one the request was made with.
    Stale,
    /// The record does not match its seal.
    Altered,
    /// The grant has ended.
    Ended,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerError {
    /// Removed, and the person was not asked.
    Voided(Voided),
    NoSuchRequest,
    /// A prompt for this request is already up.
    AlreadyAnswering,
    /// The gate refused (cooldown, caps). Retry later; the request is still pending.
    Gate(String),
    /// The store cannot be trusted or is not there. Nothing is known.
    Unavailable(String),
}

impl std::fmt::Display for AnswerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AnswerError::Voided(Voided::Stale) => {
                write!(
                    f,
                    "the plan changed since it was asked about; the request was voided"
                )
            }
            AnswerError::Voided(Voided::Altered) => {
                write!(f, "the request record was altered; it was voided")
            }
            AnswerError::Voided(Voided::Ended) => {
                write!(f, "the request's grant has ended; it was voided")
            }
            AnswerError::NoSuchRequest => write!(f, "no such pending request"),
            AnswerError::AlreadyAnswering => write!(f, "a prompt for this request is already up"),
            AnswerError::Gate(why) => write!(f, "not asked now: {why}"),
            AnswerError::Unavailable(why) => write!(f, "consent is unavailable: {why}"),
        }
    }
}

/// The durable-pending API. Methods take `&mut self` because the real store is held (and locked)
/// from open to close, and is dropped before a prompt is shown.
pub trait Consent {
    fn submit(&mut self, req: &Request, grant: &Grant, bound_hash: &str) -> Result<String, String>;
    fn begin_answer(&mut self, id: &str, bound_hash: &str) -> Result<Asking, AnswerError>;
    fn resolve(&mut self, asking: &Asking, outcome: Outcome) -> Result<Resolved, String>;
    /// When the unspent one-use approval for `(kind, subject, requester)` was given. `Err` says why
    /// there is none (never approved, or already spent). Read it **before** [`Consent::spend_one_use`]:
    /// the store never expires an approval, so freshness is the requester's to judge
    /// (`pending::APPROVAL_FRESH_SECS`).
    fn approved_at(
        &mut self,
        kind: Kind,
        subject: &str,
        requester: &Requester,
    ) -> Result<DateTime<Utc>, String>;
    /// Spends the one-use approval for `(kind, subject, requester)` **before** the action it covers,
    /// and returns the id it spent. `Err` means nothing was spent and the action must not run. Spent
    /// is durable (and audited, in the real store).
    fn spend_one_use(
        &mut self,
        kind: Kind,
        subject: &str,
        requester: &Requester,
    ) -> Result<String, String>;
    /// `false` when there was no such request.
    fn withdraw(&mut self, id: &str) -> Result<bool, String>;
    fn pending_ids(&self) -> Result<Vec<String>, String>;
}

/// One way to ask the person (Windows Hello first). The real one is `user-request`'s `Channel`.
pub trait Prompt {
    fn ask(&self, asking: &Asking) -> Outcome;
}

/// Shows the prompt and resolves it: the sequence `begin_answer`, show, `resolve`.
pub fn answer(
    consent: &mut dyn Consent,
    prompt: &dyn Prompt,
    id: &str,
    bound_hash_now: &str,
) -> Result<Resolved, AnswerError> {
    let asking = consent.begin_answer(id, bound_hash_now)?;
    let outcome = prompt.ask(&asking);
    consent
        .resolve(&asking, outcome)
        .map_err(AnswerError::Unavailable)
}

/// A backend that refuses everything: nothing is recorded, so nothing can be approved. For tests of
/// "consent is unavailable"; [`installed`] no longer returns it.
#[derive(Debug, Default)]
pub struct NoBackend;

const NO_BACKEND: &str = "no consent backend is installed, so nothing can be asked or approved";

impl Consent for NoBackend {
    fn submit(&mut self, _: &Request, _: &Grant, _: &str) -> Result<String, String> {
        Err(NO_BACKEND.into())
    }
    fn begin_answer(&mut self, _: &str, _: &str) -> Result<Asking, AnswerError> {
        Err(AnswerError::Unavailable(NO_BACKEND.into()))
    }
    fn resolve(&mut self, _: &Asking, _: Outcome) -> Result<Resolved, String> {
        Err(NO_BACKEND.into())
    }
    fn approved_at(&mut self, _: Kind, _: &str, _: &Requester) -> Result<DateTime<Utc>, String> {
        Err(NO_BACKEND.into())
    }
    fn spend_one_use(&mut self, _: Kind, _: &str, _: &Requester) -> Result<String, String> {
        Err(NO_BACKEND.into())
    }
    fn withdraw(&mut self, _: &str) -> Result<bool, String> {
        Err(NO_BACKEND.into())
    }
    fn pending_ids(&self) -> Result<Vec<String>, String> {
        Err(NO_BACKEND.into())
    }
}

/// The backend this build has: OverMind's `user-request` store, shared with every other user of it,
/// asking as agentlife (not a registered lane). `Err` when the store's place or this process cannot
/// be determined; nothing is opened here, so a store that cannot be read says so on first use.
pub fn installed() -> Result<Box<dyn Consent>, String> {
    let backend = real::UserRequestBackend::production(real::this_process()?)?;
    Ok(Box::new(backend))
}

/// A fake that enforces the semantics above, for tests. It is **not** a security component: its
/// "seal" is an unkeyed hash with a fixed salt, there for the tamper tests only.
pub mod fake {
    use super::*;
    use crate::clock::Clock;
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::fmt::Write as _;
    use std::sync::Arc;

    /// After a Cancel or a time-out the same role may not ask about the same subject for this long
    /// (the real gate's cooldown). A closed request cannot be asked again anyway; the cooldown
    /// bites a re-submitted one and a request left pending by a time-out.
    pub const COOLDOWN_SECS: i64 = 600;

    struct Record {
        id: String,
        created_at: DateTime<Utc>,
        request: Request,
        grant: Grant,
        bound_hash: String,
        seal: String,
        asking: Option<String>,
    }

    struct Approval {
        kind: Kind,
        subject: String,
        bound_hash: String,
        approved_at: DateTime<Utc>,
        spent: bool,
    }

    pub struct FakeConsent {
        clock: Arc<dyn Clock>,
        records: BTreeMap<String, Record>,
        approvals: BTreeMap<String, Approval>,
        cooldowns: BTreeMap<(String, String), DateTime<Utc>>,
        next: u64,
        /// Every id ever approved, to prove an approval spends a request exactly once.
        pub granted: Vec<String>,
        pub prompts_begun: u32,
    }

    fn seal_of(
        id: &str,
        at: DateTime<Utc>,
        req: &Request,
        grant: &Grant,
        bound_hash: &str,
    ) -> String {
        let bytes = serde_json::to_vec(&("fake-seal", id, at, req, grant, bound_hash))
            .expect("plain data serialises");
        let mut out = String::new();
        for b in Sha256::digest(&bytes).iter() {
            let _ = write!(out, "{b:02x}");
        }
        out
    }

    fn cooldown_key(req: &Request) -> (String, String) {
        (
            req.role.trim().to_lowercase(),
            req.subject.trim().to_lowercase(),
        )
    }

    impl FakeConsent {
        pub fn new(clock: Arc<dyn Clock>) -> Self {
            Self {
                clock,
                records: BTreeMap::new(),
                approvals: BTreeMap::new(),
                cooldowns: BTreeMap::new(),
                next: 0,
                granted: Vec::new(),
                prompts_begun: 0,
            }
        }

        /// Edits a stored record's subject without re-sealing it: what an altered file looks like.
        pub fn tamper_subject(&mut self, id: &str, subject: &str) {
            self.records
                .get_mut(id)
                .expect("a record to tamper with")
                .request
                .subject = subject.into();
        }

        /// A process restart: records survive, a prompt that was up does not.
        pub fn simulate_restart(&mut self) {
            for r in self.records.values_mut() {
                r.asking = None;
            }
        }

        /// The id of the unspent approval covering `(kind, subject)`, or why there is none. The oldest
        /// unspent one is used. The requester does not narrow it: a restore-plan approval is
        /// `Scope::AnyRequester` in the real store (the plan hash is the binding).
        fn unspent_for(&self, kind: Kind, subject: &str) -> Result<String, String> {
            let mut spent = false;
            for (id, a) in &self.approvals {
                if a.kind == kind && a.subject == subject {
                    if !a.spent {
                        return Ok(id.clone());
                    }
                    spent = true;
                }
            }
            Err(if spent {
                "that approval was already spent".to_string()
            } else {
                "no approval exists for that request".to_string()
            })
        }

        pub fn is_pending(&self, id: &str) -> bool {
            self.records.contains_key(id)
        }
    }

    impl Consent for FakeConsent {
        fn submit(
            &mut self,
            req: &Request,
            grant: &Grant,
            bound_hash: &str,
        ) -> Result<String, String> {
            if bound_hash.is_empty() || bound_hash.chars().any(char::is_control) {
                return Err("bound_hash must be plain and non-empty".into());
            }
            // The real crate binds a restore-plan request to its subject: `plan <hash>`, nothing else.
            if req.kind == Kind::RestorePlan {
                let in_subject = parse_restore_plan_subject(&req.subject)?;
                if in_subject != bound_hash {
                    return Err("the subject names a different plan than the bound hash".into());
                }
            }
            if self
                .approvals
                .values()
                .any(|a| !a.spent && a.bound_hash == bound_hash)
            {
                return Err("an approval for this plan is unspent; spend it first".into());
            }
            if let Some(r) = self
                .records
                .values()
                .find(|r| r.request == *req && r.grant == *grant && r.bound_hash == bound_hash)
            {
                return Ok(r.id.clone());
            }
            self.next += 1;
            let id = format!("p{:04}", self.next);
            let at = self.clock.now();
            self.records.insert(
                id.clone(),
                Record {
                    seal: seal_of(&id, at, req, grant, bound_hash),
                    id: id.clone(),
                    created_at: at,
                    request: req.clone(),
                    grant: grant.clone(),
                    bound_hash: bound_hash.into(),
                    asking: None,
                },
            );
            Ok(id)
        }

        fn begin_answer(&mut self, id: &str, bound_hash: &str) -> Result<Asking, AnswerError> {
            let now = self.clock.now();
            let Some(r) = self.records.get(id) else {
                return Err(AnswerError::NoSuchRequest);
            };
            if seal_of(&r.id, r.created_at, &r.request, &r.grant, &r.bound_hash) != r.seal {
                self.records.remove(id);
                return Err(AnswerError::Voided(Voided::Altered));
            }
            if r.bound_hash != bound_hash {
                self.records.remove(id);
                return Err(AnswerError::Voided(Voided::Stale));
            }
            if matches!(&r.grant, Grant::Until(t) if *t <= now) {
                self.records.remove(id);
                return Err(AnswerError::Voided(Voided::Ended));
            }
            if r.asking.is_some() {
                return Err(AnswerError::AlreadyAnswering);
            }
            if let Some(until) = self.cooldowns.get(&cooldown_key(&r.request)) {
                if now < *until {
                    return Err(AnswerError::Gate(format!(
                        "cooling down until {}",
                        until.format("%H:%M:%S")
                    )));
                }
            }
            let reservation = format!("res-{id}-{}", self.prompts_begun);
            self.prompts_begun += 1;
            let r = self.records.get_mut(id).expect("checked above");
            r.asking = Some(reservation.clone());
            Ok(Asking {
                pending_id: r.id.clone(),
                request: r.request.clone(),
                grant: r.grant.clone(),
                bound_hash: r.bound_hash.clone(),
                reservation,
            })
        }

        fn resolve(&mut self, asking: &Asking, outcome: Outcome) -> Result<Resolved, String> {
            let now = self.clock.now();
            let live = self
                .records
                .get(&asking.pending_id)
                .is_some_and(|r| r.asking.as_deref() == Some(asking.reservation.as_str()));
            if !live {
                return Err(
                    "that prompt is no longer open (withdrawn, voided or already resolved)".into(),
                );
            }
            let key = cooldown_key(&asking.request);
            match outcome {
                Outcome::Approved => {
                    self.records.remove(&asking.pending_id);
                    self.approvals.insert(
                        asking.pending_id.clone(),
                        Approval {
                            kind: asking.request.kind,
                            subject: asking.request.subject.clone(),
                            bound_hash: asking.bound_hash.clone(),
                            approved_at: now,
                            spent: false,
                        },
                    );
                    self.granted.push(asking.pending_id.clone());
                    Ok(Resolved::Granted)
                }
                Outcome::Cancelled => {
                    self.records.remove(&asking.pending_id);
                    self.cooldowns
                        .insert(key, now + chrono::Duration::seconds(COOLDOWN_SECS));
                    Ok(Resolved::Closed)
                }
                Outcome::TimedOut | Outcome::Unavailable => {
                    if outcome == Outcome::TimedOut {
                        self.cooldowns
                            .insert(key, now + chrono::Duration::seconds(COOLDOWN_SECS));
                    }
                    if let Some(r) = self.records.get_mut(&asking.pending_id) {
                        r.asking = None;
                    }
                    Ok(Resolved::StillPending)
                }
            }
        }

        fn approved_at(
            &mut self,
            kind: Kind,
            subject: &str,
            _requester: &Requester,
        ) -> Result<DateTime<Utc>, String> {
            let id = self.unspent_for(kind, subject)?;
            Ok(self.approvals[&id].approved_at)
        }

        fn spend_one_use(
            &mut self,
            kind: Kind,
            subject: &str,
            _requester: &Requester,
        ) -> Result<String, String> {
            let id = self.unspent_for(kind, subject)?;
            self.approvals.get_mut(&id).expect("just found").spent = true;
            Ok(id)
        }

        fn withdraw(&mut self, id: &str) -> Result<bool, String> {
            Ok(self.records.remove(id).is_some())
        }

        fn pending_ids(&self) -> Result<Vec<String>, String> {
            Ok(self.records.keys().cloned().collect())
        }
    }
}
