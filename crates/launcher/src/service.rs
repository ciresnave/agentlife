// SPDX-License-Identifier: MIT OR Apache-2.0
//! One poll cycle of the launch-on-message service, over traits: no network, no clock, no
//! process code (the same rule as the rest of this crate). `docs/LAUNCHER-SERVICE.md`, "Loop".
//!
//! The order matters and is the whole point of this module:
//!
//! 1. read the signals (a failed read still lets the outbox drain);
//! 2. decide each signal that has no unposted row yet (the daemon's signal is level-triggered, so
//!    the same signal comes back every poll until it is answered);
//! 3. post every unacknowledged result, refusals included;
//! 4. read the online set (R7) and re-check conditional refusals, clearing the ones that ended;
//! 5. collect the stall alarms.
//!
//! Wake-at entries (`docs/WAKE-AT.md`) are decided after step 4's online read, through the same engine, and never posted.
//!
//! A 401/403 from the daemon ends the cycle with `session_lost`: the caller must drop the caller
//! record at once (rule 5 of the design's Identity section) and stop answering as this role.

use crate::{Engine, LaunchResult, Outcome, Signal, Status};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceError {
    /// 401/403, or a session the daemon no longer knows: the credential is not valid now.
    Unauthorized(String),
    /// A signal that is not open any more (`409 stale_signal`). Only meaningful for a post.
    Stale,
    /// Anything else: transport, a 5xx, an unreadable reply. Retried next cycle.
    Unavailable(String),
}

/// One wake-at entry that has fallen due (`docs/WAKE-AT.md`), as the registry reader reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeDue {
    pub role: String,
    pub event: String,
    /// `<role>:wake-<compact time>`: the synthetic signal id the start is made under.
    pub signal_id: String,
    /// Unix seconds at which it fell due (its time plus the role's offset).
    pub due_ts: i64,
}

/// The daemon, as the service needs it.
pub trait Source {
    /// `GET /v1/waiting?wait=<secs>`.
    fn waiting(&self, wait_secs: u64) -> Result<Vec<Signal>, SourceError>;
    /// `POST /v1/launch-result`.
    fn post(&self, result: &LaunchResult) -> Result<(), SourceError>;
    /// `POST /v1/launch-clear`.
    fn clear(&self, role: &str) -> Result<(), SourceError>;
    /// Roles `/v1/list` shows online.
    fn online(&self) -> Result<BTreeSet<String>, SourceError>;
    /// `GET /v1/stats` `launch_senders_set`: the daemon has a sender allowlist for launch signals.
    /// Default: unknown, which the cycle treats as no allowlist (fail closed).
    fn senders_set(&self) -> Result<bool, SourceError> {
        Err(SourceError::Unavailable(
            "this source cannot read the sender allowlist".to_string(),
        ))
    }
    /// The wake-at entries due at `now`, earliest first. Default: none.
    fn due_wakes(&self, _now: i64) -> Vec<WakeDue> {
        Vec::new()
    }
    /// The launcher has dealt with this wake (started, running, or given up): drop it. Default: nothing.
    fn consume_wake(&self, _w: &WakeDue) {}
    /// Anything the source noticed since the last call that the log should carry (an entry it
    /// could not read and skipped). Default: nothing.
    fn drain_notes(&self) -> Vec<String> {
        Vec::new()
    }
}

/// Where a stall alarm goes (the portfolio PM, through AgentLife's notify path).
pub trait Notify {
    /// `true` when it reached someone.
    fn alarm(&self, text: &str) -> bool;
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Signals decided this cycle (a mechanism or a refusal; not skipped repeats).
    pub decided: usize,
    /// `<signal_id> attempt <n>` of each signal decided this cycle. The signal id ends with the
    /// enqueue time of the oldest waiting message, so this line's own timestamp minus that is the
    /// wait from mail to decision (2026-11-21 retest: ~75 s, location unmeasured).
    pub seen: Vec<String>,
    pub posted: usize,
    pub cleared: usize,
    /// Each result posted this cycle as `role=<r> outcome=<started|already-running|refused:<why>|failed:<why>>`
    /// (the lane and what became of it; `decided=1 posted=1` alone named neither, kiss 2026-11-19).
    pub results: Vec<String>,
    /// Alarm texts raised this cycle, and whether each reached anyone.
    pub alarms: Vec<(String, bool)>,
    pub session_lost: bool,
    /// Short notes for the service log: transport trouble, store trouble.
    pub notes: Vec<String>,
}

impl Report {
    /// The `cycle:` lines the service log prints for this cycle: the counts, then one line per posted
    /// result naming the lane and its outcome. Empty when the cycle decided, posted and cleared nothing.
    pub fn log_lines(&self) -> Vec<String> {
        if self.decided + self.posted + self.cleared == 0 {
            return Vec::new();
        }
        let mut lines = vec![format!(
            "cycle: decided={} posted={} cleared={}",
            self.decided, self.posted, self.cleared
        )];
        lines.extend(
            self.seen
                .iter()
                .map(|s| format!("cycle: decided signal {s}")),
        );
        lines.extend(self.results.iter().map(|r| format!("cycle: {r}")));
        lines
    }
}

fn unposted_row_exists(engine: &Engine<'_>, s: &Signal) -> bool {
    engine.data().requests.iter().any(|r| {
        r.signal_id == s.signal_id
            && r.attempt == s.attempt
            && r.epoch == s.epoch
            && !r.posted
            && r.status != Status::Intent
    })
}

/// Starts due wake-at entries through the same engine as a mail signal (caps, grant, closed-on-purpose),
/// at most [`crate::WAKES_PER_CYCLE`] per cycle. Not posted to Synapse: there is no signal to answer.
fn run_wakes(engine: &mut Engine<'_>, src: &dyn Source, now: i64, rep: &mut Report) {
    let mut starts = 0usize;
    for w in src.due_wakes(now) {
        if starts >= crate::WAKES_PER_CYCLE {
            break;
        }
        let sig = Signal {
            role: w.role.clone(),
            signal_id: w.signal_id.clone(),
            attempt: 1,
            epoch: crate::WAKE_EPOCH.to_string(),
        };
        let res = engine.handle_wake(&sig, now);
        // The wake is spent once the lane is up (or was), or is stopped on purpose; any other refusal
        // or a failure keeps it for the next cycle, until it is a day old.
        let spent = match res.outcome {
            Outcome::Started | Outcome::AlreadyRunning => true,
            Outcome::Refused => matches!(
                res.reason.as_str(),
                "closed-on-purpose" | "unknown-role" | "invalid-role"
            ),
            Outcome::Failed => false,
        };
        let expired = !spent && now.saturating_sub(w.due_ts) > crate::WAKE_EXPIRY_SECS;
        if spent || expired {
            src.consume_wake(&w);
        }
        if expired {
            rep.notes.push(format!(
                "wake expired: {} {} ({})",
                w.role,
                w.event,
                crate::result_text(&res)
            ));
        }
        if matches!(res.outcome, Outcome::Started | Outcome::Failed) {
            starts += 1;
        }
        // A refusal that will be retried is not logged every cycle: `lane-restart wakes` shows it.
        if spent || res.outcome != Outcome::Refused {
            rep.decided += 1;
            rep.seen.push(format!("{} attempt 1", w.signal_id));
            rep.results
                .push(format!("wake {}", crate::result_text(&res)));
        }
    }
}

/// Runs one cycle. `wait_secs` is the long-poll hold (0 in tests); `launch_wait` is Synapse's.
#[must_use]
pub fn cycle(
    engine: &mut Engine<'_>,
    src: &dyn Source,
    notify: &dyn Notify,
    now: i64,
    launch_wait: i64,
    wait_secs: u64,
) -> Report {
    let mut rep = Report::default();

    // Read before any decision, every cycle: an allowlist emptied since the last one must stop
    // launches at once. An unreadable answer is "not confirmed", never "confirmed".
    match src.senders_set() {
        Ok(set) => engine.set_allowlist_confirmed(set),
        Err(SourceError::Unauthorized(w)) => {
            engine.set_allowlist_confirmed(false);
            rep.session_lost = true;
            rep.notes.push(format!("stats: unauthorized: {w}"));
            return rep;
        }
        Err(e) => {
            engine.set_allowlist_confirmed(false);
            rep.notes.push(format!("stats: {e:?}"));
        }
    }

    let waiting = src.waiting(wait_secs);
    rep.notes.extend(src.drain_notes());
    match waiting {
        Ok(signals) => {
            for s in &signals {
                if unposted_row_exists(engine, s) {
                    continue;
                }
                let _ = engine.handle(s, now);
                rep.decided += 1;
                rep.seen
                    .push(format!("{} attempt {}", s.signal_id, s.attempt));
            }
        }
        Err(SourceError::Unauthorized(w)) => {
            rep.session_lost = true;
            rep.notes.push(format!("waiting: unauthorized: {w}"));
            return rep;
        }
        Err(e) => rep.notes.push(format!("waiting: {e:?}")),
    }

    for r in engine.outbox() {
        match src.post(&r) {
            Ok(()) | Err(SourceError::Stale) => match engine.mark_posted(&r) {
                Ok(()) => {
                    rep.posted += 1;
                    let mut text = crate::result_text(&r);
                    if r.reason == "rate" {
                        text.push_str(&format!(" ({})", engine.rate_text(now, &r.role)));
                    }
                    rep.results.push(text);
                }
                Err(e) => rep.notes.push(format!("store: {e}")),
            },
            Err(SourceError::Unauthorized(w)) => {
                rep.session_lost = true;
                rep.notes.push(format!("post: unauthorized: {w}"));
                return rep;
            }
            Err(e) => rep.notes.push(format!("post {}: {e:?}", r.role)),
        }
    }

    match src.online() {
        Ok(online) => {
            if let Err(e) = engine.observe_online(&online, now) {
                rep.notes.push(format!("store: {e}"));
            }
        }
        Err(SourceError::Unauthorized(w)) => {
            rep.session_lost = true;
            rep.notes.push(format!("list: unauthorized: {w}"));
            return rep;
        }
        Err(e) => rep.notes.push(format!("list: {e:?}")),
    }

    // After the online read, so a lane just seen online has its rule-5 counter reset before its wake is decided.
    run_wakes(engine, src, now, &mut rep);

    let to_clear = match engine.recheck(now) {
        Ok(roles) => roles,
        Err(e) => {
            // The `Clearing` marks are not durable: clearing now could strand a refusal after a
            // crash, so wait for the next cycle.
            rep.notes
                .push(format!("store: not clearing this cycle: {e}"));
            Vec::new()
        }
    };
    for role in to_clear {
        match src.clear(&role) {
            Ok(()) => match engine.cleared(&role) {
                Ok(()) => rep.cleared += 1,
                Err(e) => rep.notes.push(format!("store: {e}")),
            },
            Err(SourceError::Unauthorized(w)) => {
                rep.session_lost = true;
                rep.notes.push(format!("clear: unauthorized: {w}"));
                return rep;
            }
            Err(e) => rep.notes.push(format!("clear {role}: {e:?}")),
        }
    }

    // An alarm is marked raised only after it reached someone, so a failed delivery is offered
    // again next cycle instead of being lost for the whole stall episode.
    for (role, text) in engine.pending_alarms(now, launch_wait) {
        let reached = notify.alarm(&text);
        if reached {
            if let Err(e) = engine.mark_alarmed(&role) {
                rep.notes.push(format!("store: {e}"));
            }
        }
        rep.alarms.push((text, reached));
    }
    rep
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Directory, Lane, MechResult, Mechanism, Outcome, StoreData, StoreError, StoreIo};
    use std::cell::RefCell;

    struct Mem(RefCell<Option<StoreData>>);
    impl StoreIo for Mem {
        fn load(&self) -> Result<Option<StoreData>, StoreError> {
            Ok(self.0.borrow().clone())
        }
        fn save(&self, d: &StoreData) -> Result<(), StoreError> {
            *self.0.borrow_mut() = Some(d.clone());
            Ok(())
        }
    }

    /// Counts calls: a notifier-only service must never reach it.
    struct Mech(RefCell<u32>);
    impl Mechanism for Mech {
        fn start(&self, _: &str, _: &str, _: &str) -> MechResult {
            *self.0.borrow_mut() += 1;
            MechResult::Started
        }
    }

    struct Dir {
        granted: RefCell<bool>,
    }
    impl Directory for Dir {
        fn lane(&self, role: &str) -> Lane {
            match role {
                "fuel" => Lane::Launchable,
                r if r.starts_with('r') => Lane::Launchable,
                "stopped" => Lane::ClosedOnPurpose,
                _ => Lane::Unknown,
            }
        }
        fn granted(&self, _: &str) -> bool {
            *self.granted.borrow()
        }
        fn running(&self, _: &str) -> bool {
            false
        }
    }

    #[derive(Default)]
    struct Src {
        signals: RefCell<Vec<Signal>>,
        posted: RefCell<Vec<LaunchResult>>,
        cleared: RefCell<Vec<String>>,
        online: RefCell<BTreeSet<String>>,
        waiting_err: RefCell<Option<SourceError>>,
        post_err: RefCell<Option<SourceError>>,
        /// What `senders_set` answers: `None` is an unreadable answer.
        senders: RefCell<Option<bool>>,
        wakes: RefCell<Vec<WakeDue>>,
    }
    impl Source for Src {
        fn due_wakes(&self, now: i64) -> Vec<WakeDue> {
            self.wakes
                .borrow()
                .iter()
                .filter(|w| w.due_ts <= now)
                .cloned()
                .collect()
        }
        fn consume_wake(&self, w: &WakeDue) {
            self.wakes.borrow_mut().retain(|x| x != w);
        }
        fn senders_set(&self) -> Result<bool, SourceError> {
            (*self.senders.borrow()).ok_or_else(|| SourceError::Unavailable("stats".into()))
        }
        fn waiting(&self, _: u64) -> Result<Vec<Signal>, SourceError> {
            match self.waiting_err.borrow().clone() {
                Some(e) => Err(e),
                None => Ok(self.signals.borrow().clone()),
            }
        }
        fn post(&self, r: &LaunchResult) -> Result<(), SourceError> {
            if let Some(e) = self.post_err.borrow().clone() {
                return Err(e);
            }
            self.posted.borrow_mut().push(r.clone());
            Ok(())
        }
        fn clear(&self, role: &str) -> Result<(), SourceError> {
            self.cleared.borrow_mut().push(role.into());
            Ok(())
        }
        fn online(&self) -> Result<BTreeSet<String>, SourceError> {
            Ok(self.online.borrow().clone())
        }
    }

    struct Bell(RefCell<Vec<String>>);
    impl Notify for Bell {
        fn alarm(&self, t: &str) -> bool {
            self.0.borrow_mut().push(t.into());
            true
        }
    }

    fn signal(role: &str, id: &str, attempt: u32, epoch: &str) -> Signal {
        Signal {
            role: role.into(),
            signal_id: id.into(),
            attempt,
            epoch: epoch.into(),
        }
    }

    struct Rig {
        io: Mem,
        mech: Mech,
        dir: Dir,
        src: Src,
        bell: Bell,
    }
    fn rig() -> Rig {
        Rig {
            io: Mem(RefCell::new(None)),
            mech: Mech(RefCell::new(0)),
            dir: Dir {
                granted: RefCell::new(false),
            },
            src: Src {
                senders: RefCell::new(Some(true)),
                ..Src::default()
            },
            bell: Bell(RefCell::new(Vec::new())),
        }
    }

    fn wake(role: &str, event: &str, due_ts: i64) -> WakeDue {
        WakeDue {
            role: role.into(),
            event: event.into(),
            signal_id: format!("{role}:wake-{event}"),
            due_ts,
        }
    }

    // docs/WAKE-AT.md. A due wake starts the lane once, is consumed, posts nothing to Synapse, and needs
    // no sender allowlist (there is no sender).
    #[test]
    fn a_due_wake_starts_the_lane_once_and_posts_nothing() {
        let r = rig();
        *r.dir.granted.borrow_mut() = true;
        *r.src.senders.borrow_mut() = None; // allowlist unreadable: mail would be refused
        r.src.wakes.borrow_mut().push(wake("fuel", "e1", 50));
        let mut e = Engine::open(&r.io, &r.mech, &r.dir, true).unwrap();
        let a = cycle(&mut e, &r.src, &r.bell, 100, 120, 0);
        assert_eq!(*r.mech.0.borrow(), 1);
        assert!(r.src.wakes.borrow().is_empty(), "consumed");
        assert!(
            r.src.posted.borrow().is_empty(),
            "nothing posted to Synapse"
        );
        assert!(e.outbox().is_empty());
        assert_eq!(a.decided, 1);
        assert!(
            a.log_lines()
                .iter()
                .any(|l| l == "cycle: wake role=fuel outcome=started"),
            "{:?}",
            a.log_lines()
        );
        let b = cycle(&mut e, &r.src, &r.bell, 105, 120, 0);
        assert_eq!(*r.mech.0.borrow(), 1, "a second cycle starts nothing");
        assert_eq!(b.decided, 0);
        assert!(e.data().requests.iter().all(|x| x.posted));
    }

    #[test]
    fn a_wake_not_yet_due_does_nothing() {
        let r = rig();
        *r.dir.granted.borrow_mut() = true;
        r.src.wakes.borrow_mut().push(wake("fuel", "e1", 500));
        let mut e = Engine::open(&r.io, &r.mech, &r.dir, true).unwrap();
        let _ = cycle(&mut e, &r.src, &r.bell, 100, 120, 0);
        assert_eq!(*r.mech.0.borrow(), 0);
        assert_eq!(r.src.wakes.borrow().len(), 1);
    }

    // The lane was already started by mail (or never stopped): no double start, the wake is spent.
    #[test]
    fn a_wake_for_a_running_lane_is_consumed_without_a_second_start() {
        struct Running(RefCell<u32>);
        impl Mechanism for Running {
            fn start(&self, _: &str, _: &str, _: &str) -> MechResult {
                *self.0.borrow_mut() += 1;
                MechResult::AlreadyRunning
            }
        }
        let r = rig();
        *r.dir.granted.borrow_mut() = true;
        r.src.wakes.borrow_mut().push(wake("fuel", "e1", 50));
        let m = Running(RefCell::new(0));
        let mut e = Engine::open(&r.io, &m, &r.dir, true).unwrap();
        let a = cycle(&mut e, &r.src, &r.bell, 100, 120, 0);
        assert!(r.src.wakes.borrow().is_empty());
        assert!(a
            .log_lines()
            .iter()
            .any(|l| l == "cycle: wake role=fuel outcome=already-running"));
        let _ = cycle(&mut e, &r.src, &r.bell, 105, 120, 0);
        assert_eq!(*m.0.borrow(), 1);
    }

    #[test]
    fn a_wake_for_a_lane_stopped_on_purpose_is_consumed_and_not_started() {
        let r = rig();
        *r.dir.granted.borrow_mut() = true;
        r.src.wakes.borrow_mut().push(wake("stopped", "e1", 50));
        let mut e = Engine::open(&r.io, &r.mech, &r.dir, true).unwrap();
        let a = cycle(&mut e, &r.src, &r.bell, 100, 120, 0);
        assert_eq!(*r.mech.0.borrow(), 0);
        assert!(r.src.wakes.borrow().is_empty());
        assert!(
            a.log_lines()
                .iter()
                .any(|l| l == "cycle: wake role=stopped outcome=refused:closed-on-purpose"),
            "{:?}",
            a.log_lines()
        );
    }

    // Five lanes due at once start two per cycle; none is lost.
    #[test]
    fn a_burst_of_wakes_spreads_over_cycles_and_none_is_lost() {
        let r = rig();
        *r.dir.granted.borrow_mut() = true;
        for i in 1..=5 {
            r.src
                .wakes
                .borrow_mut()
                .push(wake(&format!("r{i}"), "e", 50));
        }
        let mut e = Engine::open(&r.io, &r.mech, &r.dir, true).unwrap();
        let mut started = Vec::new();
        for (i, now) in [100, 130, 160, 190].into_iter().enumerate() {
            let _ = cycle(&mut e, &r.src, &r.bell, now, 120, 0);
            started.push(*r.mech.0.borrow());
            assert!(i < 3 || r.src.wakes.borrow().is_empty());
        }
        assert_eq!(started, vec![2, 4, 5, 5]);
        assert!(r.src.wakes.borrow().is_empty());
    }

    // Four wakes for one lane: the 4th meets the cap and is KEPT, then starts once the hour has passed
    // and the lane has been seen online (rule 5).
    #[test]
    fn a_wake_refused_by_a_cap_is_kept_and_starts_when_the_cap_clears() {
        let r = rig();
        *r.dir.granted.borrow_mut() = true;
        for i in 1..=4 {
            r.src
                .wakes
                .borrow_mut()
                .push(wake("fuel", &format!("e{i}"), 50));
        }
        let mut e = Engine::open(&r.io, &r.mech, &r.dir, true).unwrap();
        let _ = cycle(&mut e, &r.src, &r.bell, 100, 120, 0);
        let _ = cycle(&mut e, &r.src, &r.bell, 130, 120, 0);
        assert_eq!(*r.mech.0.borrow(), 3);
        assert_eq!(r.src.wakes.borrow().len(), 1, "the 4th is kept");
        assert!(
            r.src.cleared.borrow().is_empty(),
            "no launch-clear for a wake"
        );
        r.src.online.borrow_mut().insert("fuel".into());
        let _ = cycle(&mut e, &r.src, &r.bell, 100 + 3700, 120, 0);
        assert_eq!(*r.mech.0.borrow(), 4);
        assert!(r.src.wakes.borrow().is_empty());
        assert!(r.src.cleared.borrow().is_empty());
    }

    #[test]
    fn a_wake_that_stays_refused_for_a_day_is_given_up_with_a_note() {
        let r = rig();
        // no grant: refused, kept ...
        r.src.wakes.borrow_mut().push(wake("fuel", "e1", 50));
        let mut e = Engine::open(&r.io, &r.mech, &r.dir, true).unwrap();
        let a = cycle(&mut e, &r.src, &r.bell, 100, 120, 0);
        assert_eq!(r.src.wakes.borrow().len(), 1);
        assert_eq!(*r.mech.0.borrow(), 0);
        assert!(!a.notes.iter().any(|n| n.contains("expired")));
        // ... until a day past its due time
        let b = cycle(
            &mut e,
            &r.src,
            &r.bell,
            50 + crate::WAKE_EXPIRY_SECS + 1,
            120,
            0,
        );
        assert!(r.src.wakes.borrow().is_empty());
        assert!(
            b.notes
                .iter()
                .any(|n| n.contains("wake expired") && n.contains("fuel")),
            "{:?}",
            b.notes
        );
    }

    // A mail signal still needs the allowlist: only a wake is exempt, and only with the wake epoch.
    #[test]
    fn a_mail_signal_still_needs_the_allowlist() {
        let r = rig();
        *r.dir.granted.borrow_mut() = true;
        *r.src.senders.borrow_mut() = Some(false);
        r.src
            .signals
            .borrow_mut()
            .push(signal("fuel", "fuel:wake-spoof", 1, "ep-A"));
        let mut e = Engine::open(&r.io, &r.mech, &r.dir, true).unwrap();
        let _ = cycle(&mut e, &r.src, &r.bell, 100, 120, 0);
        assert_eq!(*r.mech.0.borrow(), 0);
        assert_eq!(r.src.posted.borrow()[0].reason, "no-sender-allowlist");
    }

    // A notifier-only service answers `refused: no-grant`, echoes the epoch string verbatim and
    // never calls the mechanism.
    #[test]
    fn without_a_grant_a_signal_is_refused_with_its_epoch_and_the_mechanism_is_not_called() {
        let r = rig();
        r.src
            .signals
            .borrow_mut()
            .push(signal("fuel", "s1", 1, "ep-A"));
        let mut e = Engine::open(&r.io, &r.mech, &r.dir, true).unwrap();
        let rep = cycle(&mut e, &r.src, &r.bell, 100, 120, 0);
        assert_eq!((rep.decided, rep.posted), (1, 1));
        // the log names the lane and what became of it, not only the counts
        assert_eq!(rep.results.len(), 1, "{:?}", rep.results);
        assert!(
            rep.results[0].starts_with("role=fuel outcome=refused:"),
            "{:?}",
            rep.results
        );
        let posted = r.src.posted.borrow();
        assert_eq!(posted.len(), 1);
        assert_eq!(posted[0].epoch, "ep-A");
        assert_eq!(posted[0].outcome, Outcome::Refused);
        assert_eq!(posted[0].reason, "no-grant");
        assert_eq!(*r.mech.0.borrow(), 0);
    }

    // The signal is level-triggered: the same one next cycle is not decided again while its row
    // is unposted, and a daemon that is down leaves the result in the outbox.
    #[test]
    fn a_repeated_signal_is_decided_once_and_posted_once() {
        let r = rig();
        r.src
            .signals
            .borrow_mut()
            .push(signal("fuel", "s1", 1, "e"));
        *r.src.post_err.borrow_mut() = Some(SourceError::Unavailable("down".into()));
        let mut e = Engine::open(&r.io, &r.mech, &r.dir, true).unwrap();
        let a = cycle(&mut e, &r.src, &r.bell, 100, 120, 0);
        assert_eq!(
            (a.decided, a.posted),
            (1, 0),
            "daemon down: stays in the outbox"
        );
        let b = cycle(&mut e, &r.src, &r.bell, 105, 120, 0);
        assert_eq!(b.decided, 0, "no second row for the same unposted key");
        assert_eq!(e.data().requests.len(), 1);
        *r.src.post_err.borrow_mut() = None;
        let c = cycle(&mut e, &r.src, &r.bell, 110, 120, 0);
        assert_eq!(c.posted, 1);
        assert_eq!(r.src.posted.borrow().len(), 1);
        assert!(e.outbox().is_empty());
    }

    // `stale_signal` means the signal is gone: nothing left to say, so it is acknowledged.
    #[test]
    fn a_stale_signal_reply_counts_as_posted() {
        let r = rig();
        r.src
            .signals
            .borrow_mut()
            .push(signal("fuel", "s1", 1, "e"));
        *r.src.post_err.borrow_mut() = Some(SourceError::Stale);
        let mut e = Engine::open(&r.io, &r.mech, &r.dir, true).unwrap();
        let rep = cycle(&mut e, &r.src, &r.bell, 100, 120, 0);
        assert_eq!(rep.posted, 1);
        assert!(e.outbox().is_empty());
    }

    // 401/403 ends the cycle at once and is reported, so the caller can drop its record.
    #[test]
    fn an_unauthorized_reply_ends_the_cycle_and_reports_session_lost() {
        let r = rig();
        *r.src.waiting_err.borrow_mut() = Some(SourceError::Unauthorized("403".into()));
        let mut e = Engine::open(&r.io, &r.mech, &r.dir, true).unwrap();
        let rep = cycle(&mut e, &r.src, &r.bell, 100, 120, 0);
        assert!(rep.session_lost);
        assert!(r.src.posted.borrow().is_empty());
    }

    // A refusal for a missing grant is cleared (launch-clear) once the grant appears.
    #[test]
    fn a_no_grant_refusal_is_cleared_when_the_grant_appears() {
        let r = rig();
        r.src
            .signals
            .borrow_mut()
            .push(signal("fuel", "s1", 1, "e"));
        let mut e = Engine::open(&r.io, &r.mech, &r.dir, true).unwrap();
        let _ = cycle(&mut e, &r.src, &r.bell, 100, 120, 0);
        assert!(r.src.cleared.borrow().is_empty());
        *r.dir.granted.borrow_mut() = true;
        let rep = cycle(&mut e, &r.src, &r.bell, 110, 120, 0);
        assert_eq!(rep.cleared, 1);
        assert_eq!(r.src.cleared.borrow().as_slice(), ["fuel"]);
    }

    // An unknown role is a terminal refusal: posted once, never launched, never cleared.
    #[test]
    fn an_unknown_role_is_refused_terminally() {
        let r = rig();
        r.src
            .signals
            .borrow_mut()
            .push(signal("ghost", "g1", 1, "e"));
        let mut e = Engine::open(&r.io, &r.mech, &r.dir, true).unwrap();
        let _ = cycle(&mut e, &r.src, &r.bell, 100, 120, 0);
        let rep = cycle(&mut e, &r.src, &r.bell, 110, 120, 0);
        assert_eq!(rep.cleared, 0);
        assert_eq!(r.src.posted.borrow()[0].reason, "unknown-role");
        assert_eq!(*r.mech.0.borrow(), 0);
    }

    /// A bell that can be made to fail.
    struct FlakyBell {
        works: RefCell<bool>,
        rang: RefCell<u32>,
    }
    impl Notify for FlakyBell {
        fn alarm(&self, _: &str) -> bool {
            *self.rang.borrow_mut() += 1;
            *self.works.borrow()
        }
    }

    // A stall alarm that did not reach anyone is offered again next cycle; once delivered it is
    // not raised again for the same stall.
    #[test]
    fn an_alarm_that_failed_to_deliver_is_retried_then_raised_once() {
        let r = rig();
        *r.dir.granted.borrow_mut() = true;
        r.src
            .signals
            .borrow_mut()
            .push(signal("fuel", "s1", 1, "e"));
        let mut e = Engine::open(&r.io, &r.mech, &r.dir, true).unwrap();
        let _ = cycle(&mut e, &r.src, &r.bell, 100, 120, 0); // started
        r.src.signals.borrow_mut().clear();
        let bell = FlakyBell {
            works: RefCell::new(false),
            rang: RefCell::new(0),
        };
        let a = cycle(&mut e, &r.src, &bell, 100 + 241, 120, 0);
        assert_eq!(a.alarms.len(), 1);
        assert!(!a.alarms[0].1, "delivery failed");
        *bell.works.borrow_mut() = true;
        let b = cycle(&mut e, &r.src, &bell, 100 + 242, 120, 0);
        assert_eq!(b.alarms.len(), 1, "offered again");
        assert!(b.alarms[0].1);
        let c = cycle(&mut e, &r.src, &bell, 100 + 243, 120, 0);
        assert!(c.alarms.is_empty(), "raised once, after it was delivered");
        assert_eq!(*bell.rang.borrow(), 2);
    }

    #[test]
    fn a_granted_signal_waits_for_the_daemon_to_confirm_a_sender_allowlist_and_a_failed_read_is_not_a_yes(
    ) {
        for (answer, launches, note) in [
            (Some(true), true, false),
            (Some(false), false, false),
            (None, false, true),
        ] {
            let r = rig();
            *r.dir.granted.borrow_mut() = true;
            *r.src.senders.borrow_mut() = answer;
            r.src
                .signals
                .borrow_mut()
                .push(signal("fuel", "s1", 1, "e"));
            let mut e = Engine::open(&r.io, &r.mech, &r.dir, true).unwrap();
            let rep = cycle(&mut e, &r.src, &r.bell, 100, 120, 0);
            assert_eq!(*r.mech.0.borrow() == 1, launches, "{answer:?}");
            let posted = r.src.posted.borrow();
            assert_eq!(posted.len(), 1, "{answer:?}");
            assert_eq!(
                posted[0].reason,
                if launches {
                    "started"
                } else {
                    "no-sender-allowlist"
                },
                "{answer:?}"
            );
            assert_eq!(
                rep.notes.iter().any(|n| n.starts_with("stats:")),
                note,
                "{answer:?}"
            );
        }
    }

    #[test]
    fn a_source_that_cannot_read_the_allowlist_never_confirms_it() {
        struct Blind;
        impl Source for Blind {
            fn waiting(&self, _: u64) -> Result<Vec<Signal>, SourceError> {
                Ok(vec![])
            }
            fn post(&self, _: &LaunchResult) -> Result<(), SourceError> {
                Ok(())
            }
            fn clear(&self, _: &str) -> Result<(), SourceError> {
                Ok(())
            }
            fn online(&self) -> Result<BTreeSet<String>, SourceError> {
                Ok(BTreeSet::new())
            }
        }
        assert!(matches!(
            Blind.senders_set(),
            Err(SourceError::Unavailable(_))
        ));
    }
}
