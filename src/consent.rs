// SPDX-License-Identifier: MIT OR Apache-2.0
//! Consent (M4): the shape of OverMind's `user-request` 0.8.0 durable-pending API, as a trait.
//!
//! **Why a trait and not the crate.** `user-request` is not on crates.io (measured 2026-10-08,
//! `cargo info user-request`: not found), and CireSnave's Sources rule forbids a path or `git =`
//! dependency (`CLAUDE.md` §9). The PM ruled (2026-10-08): build against a trait shaped like the
//! crate's API, test against a fake that enforces its semantics, and wire the real crate when it is
//! published (board 143 moves it). Nothing from OverMind is copied here; the shape below was read
//! from its README and `store/pending.rs` at OverMind `main` after #125.
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

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
    /// Who asks, by role. The real crate takes this from the process table, never an argument.
    pub role: String,
    pub subject: String,
    pub summary: String,
    pub reason: String,
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

/// What spending a one-use approval returns. The store never expires an unspent approval, so the
/// requester judges freshness from `approved_at` itself (`pending::APPROVAL_FRESH_SECS`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spent {
    pub approved_at: DateTime<Utc>,
    pub bound_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpendError {
    /// Nothing was approved under that id.
    NoApproval,
    /// Already spent: an approval is used once.
    AlreadySpent,
    /// The approval is for a different plan; it is left unspent.
    Stale,
    Unavailable(String),
}

impl std::fmt::Display for SpendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpendError::NoApproval => write!(f, "no approval exists for that request"),
            SpendError::AlreadySpent => write!(f, "that approval was already spent"),
            SpendError::Stale => write!(f, "the approval is for a different plan"),
            SpendError::Unavailable(why) => write!(f, "consent is unavailable: {why}"),
        }
    }
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
    /// Spends the one-use approval of request `id`, **before** the action it covers; the action runs
    /// only on `Ok`. Spent is durable (and audited, in the real store). `bound_hash` is the plan held
    /// now; a different one is refused and leaves the approval unspent.
    fn spend_one_use(&mut self, id: &str, bound_hash: &str) -> Result<Spent, SpendError>;
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

/// The backend until `user-request` is published: every call says so. Nothing is recorded, so
/// nothing can be approved.
#[derive(Debug, Default)]
pub struct NoBackend;

const NO_BACKEND: &str = "no consent backend is installed: OverMind's user-request crate is not \
published yet (board 143), so nothing can be asked or approved";

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
    fn spend_one_use(&mut self, _: &str, _: &str) -> Result<Spent, SpendError> {
        Err(SpendError::Unavailable(NO_BACKEND.into()))
    }
    fn withdraw(&mut self, _: &str) -> Result<bool, String> {
        Err(NO_BACKEND.into())
    }
    fn pending_ids(&self) -> Result<Vec<String>, String> {
        Err(NO_BACKEND.into())
    }
}

/// The backend this build has. **The one place to wire `user-request`** once it is published: until
/// then there is none, and every command that needs consent says so and changes nothing.
pub fn installed() -> Result<Box<dyn Consent>, String> {
    Err(NO_BACKEND.into())
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
        role: String,
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
            if self.approvals.values().any(|a| {
                !a.spent && a.bound_hash == bound_hash && a.role == req.role.trim().to_lowercase()
            }) {
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
                            role: asking.request.role.trim().to_lowercase(),
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

        fn spend_one_use(&mut self, id: &str, bound_hash: &str) -> Result<Spent, SpendError> {
            let a = self.approvals.get_mut(id).ok_or(SpendError::NoApproval)?;
            if a.spent {
                return Err(SpendError::AlreadySpent);
            }
            if a.bound_hash != bound_hash {
                return Err(SpendError::Stale);
            }
            a.spent = true;
            Ok(Spent {
                approved_at: a.approved_at,
                bound_hash: a.bound_hash.clone(),
            })
        }

        fn withdraw(&mut self, id: &str) -> Result<bool, String> {
            Ok(self.records.remove(id).is_some())
        }

        fn pending_ids(&self) -> Result<Vec<String>, String> {
            Ok(self.records.keys().cloned().collect())
        }
    }
}
