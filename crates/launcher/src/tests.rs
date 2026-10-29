use super::*;
use std::cell::{Cell, RefCell};

/// A stand-in mechanism: scripted answers, every call recorded.
struct Mech {
    calls: RefCell<Vec<(String, String)>>,
    answers: RefCell<Vec<MechResult>>,
}

impl Mech {
    fn new(answers: Vec<MechResult>) -> Self {
        Mech {
            calls: RefCell::new(Vec::new()),
            answers: RefCell::new(answers),
        }
    }
    fn n(&self) -> usize {
        self.calls.borrow().len()
    }
}

impl Mechanism for Mech {
    fn start(&self, role: &str, model: &str, _signal_id: &str) -> MechResult {
        self.calls.borrow_mut().push((role.into(), model.into()));
        let mut a = self.answers.borrow_mut();
        if a.len() > 1 {
            a.remove(0)
        } else {
            a[0].clone()
        }
    }
}

struct Dir {
    unknown: BTreeSet<String>,
    closed: BTreeSet<String>,
    grant: Cell<bool>,
    running: Cell<bool>,
}

impl Dir {
    fn new() -> Self {
        Dir {
            unknown: BTreeSet::new(),
            closed: BTreeSet::new(),
            grant: Cell::new(true),
            running: Cell::new(false),
        }
    }
}

impl Directory for Dir {
    fn lane(&self, role: &str) -> Lane {
        if self.unknown.contains(role) {
            Lane::Unknown
        } else if self.closed.contains(role) {
            Lane::ClosedOnPurpose
        } else {
            Lane::Launchable
        }
    }
    fn granted(&self, _: &str) -> bool {
        self.grant.get()
    }
    fn running(&self, _: &str) -> bool {
        self.running.get()
    }
}

/// An in-memory store that can be made to fail, to read what a restart sees.
struct Mem {
    data: RefCell<Option<StoreData>>,
    fail_save: Cell<bool>,
    fail_load: Cell<bool>,
}

impl Mem {
    fn new() -> Self {
        Mem {
            data: RefCell::new(None),
            fail_save: Cell::new(false),
            fail_load: Cell::new(false),
        }
    }
}

impl StoreIo for Mem {
    fn load(&self) -> Result<Option<StoreData>, StoreError> {
        if self.fail_load.get() {
            return Err(StoreError::Unreadable("corrupt".into()));
        }
        Ok(self.data.borrow().clone())
    }
    fn save(&self, d: &StoreData) -> Result<(), StoreError> {
        if self.fail_save.get() {
            return Err(StoreError::Write("disk".into()));
        }
        *self.data.borrow_mut() = Some(d.clone());
        Ok(())
    }
}

fn sig(role: &str, id: u64, attempt: u32) -> Signal {
    Signal {
        role: role.into(),
        signal_id: format!("{role}:{id}"),
        attempt,
        epoch: String::new(),
    }
}

fn open<'a>(io: &'a Mem, m: &'a Mech, d: &'a Dir) -> Engine<'a> {
    let mut e = Engine::open(io, m, d, true).unwrap();
    e.set_allowlist_confirmed(true);
    e
}

fn started() -> Mech {
    Mech::new(vec![MechResult::Started])
}

fn res(o: Outcome, r: &str) -> (Outcome, String) {
    (o, r.to_string())
}

fn got(r: &LaunchResult) -> (Outcome, String) {
    (r.outcome, r.reason.clone())
}

// (a) the same signal twice inside the window launches once
#[test]
fn a_replay_inside_the_window_launches_once_and_answers_the_recorded_result() {
    let (io, m, d) = (Mem::new(), started(), Dir::new());
    let mut e = open(&io, &m, &d);
    let r1 = e.handle(&sig("fuel", 5, 1), 1000);
    assert_eq!(got(&r1), res(Outcome::Started, "started"));
    let r2 = e.handle(&sig("fuel", 5, 1), 1100);
    assert_eq!(r2, r1);
    assert_eq!(m.n(), 1, "one mechanism call");
}

// (b) the same key after the window re-evaluates and launches (a new generation)
#[test]
fn the_same_key_after_the_window_is_a_new_request_and_keeps_the_old_row() {
    let (io, m, d) = (Mem::new(), started(), Dir::new());
    let mut e = open(&io, &m, &d);
    e.handle(&sig("fuel", 5, 1), 1000);
    e.handle(&sig("fuel", 5, 1), 1000 + REPLAY_WINDOW_SECS + 1);
    assert_eq!(m.n(), 2);
    let gens: Vec<u32> = e.data().requests.iter().map(|r| r.generation).collect();
    assert_eq!(gens, [0, 1], "no overwrite, no collision");
}

// (c) attempt+1 of a failed signal launches again
#[test]
fn a_higher_attempt_is_a_new_request() {
    let (io, d) = (Mem::new(), Dir::new());
    let m = Mech::new(vec![MechResult::Failed("x".into()), MechResult::Started]);
    let mut e = open(&io, &m, &d);
    assert_eq!(
        got(&e.handle(&sig("fuel", 5, 1), 1000)),
        res(Outcome::Failed, "spawn-failed")
    );
    assert_eq!(
        got(&e.handle(&sig("fuel", 5, 2), 1010)),
        res(Outcome::Started, "started")
    );
    assert_eq!(m.n(), 2);
}

// An epoch change is a new request, never a replay.
#[test]
fn a_changed_epoch_is_not_a_replay() {
    let (io, m, d) = (Mem::new(), started(), Dir::new());
    let mut e = open(&io, &m, &d);
    let mut s = sig("fuel", 5, 1);
    e.handle(&s, 1000);
    s.epoch = "1".into();
    e.handle(&s, 1010);
    assert_eq!(m.n(), 2);
}

// (d) an unknown role never reaches the mechanism; (closed on purpose likewise); invalid role
#[test]
fn unknown_closed_and_invalid_roles_never_reach_the_mechanism() {
    let (io, m, mut d) = (Mem::new(), started(), Dir::new());
    d.unknown.insert("ghost".into());
    d.closed.insert("parked".into());
    let mut e = open(&io, &m, &d);
    assert_eq!(
        got(&e.handle(&sig("ghost", 1, 1), 10)),
        res(Outcome::Refused, "unknown-role")
    );
    assert_eq!(
        got(&e.handle(&sig("parked", 1, 1), 10)),
        res(Outcome::Refused, "closed-on-purpose")
    );
    let bad = Signal {
        role: "a;b".into(),
        signal_id: "a;b:1".into(),
        attempt: 1,
        epoch: String::new(),
    };
    assert_eq!(
        got(&e.handle(&bad, 10)),
        res(Outcome::Refused, "invalid-role")
    );
    assert_eq!(m.n(), 0);
    // positive control: a known, open lane on the same engine does launch
    assert_eq!(
        got(&e.handle(&sig("fuel", 1, 1), 10)),
        res(Outcome::Started, "started")
    );
    assert_eq!(m.n(), 1);
}

// (e) the 4th spawn for one role in an hour is refused:rate; the 13th overall too; refusals and
// replays consume nothing
#[test]
fn rate_caps_bound_spawns_per_role_and_overall() {
    let (io, m, d) = (Mem::new(), started(), Dir::new());
    let mut e = open(&io, &m, &d);
    // a role seen online between launches, so rule 5 does not interfere
    let online = |r: &str| BTreeSet::from([r.to_string()]);
    for i in 0..RATE_PER_ROLE as u64 {
        assert_eq!(
            got(&e.handle(&sig("fuel", i, 1), 100 + i as i64)),
            res(Outcome::Started, "started")
        );
        e.observe_online(&online("fuel"), 100 + i as i64).unwrap();
    }
    assert_eq!(
        got(&e.handle(&sig("fuel", 9, 1), 200)),
        res(Outcome::Refused, "rate")
    );
    assert_eq!(
        m.n(),
        RATE_PER_ROLE,
        "the refusal did not call the mechanism"
    );
    // overall cap: other roles fill it up to 12, the 13th is refused
    for k in 0..(RATE_OVERALL - RATE_PER_ROLE) {
        let role = format!("r{k}");
        assert_eq!(
            got(&e.handle(&sig(&role, 1, 1), 300 + k as i64)).0,
            Outcome::Started
        );
    }
    assert_eq!(
        got(&e.handle(&sig("last", 1, 1), 400)),
        res(Outcome::Refused, "rate")
    );
}

// (f) after the window frees a slot the service clears the role itself
#[test]
fn a_rate_refusal_is_cleared_once_a_slot_frees_and_deleted_only_after_the_clear_succeeds() {
    let (io, m, d) = (Mem::new(), started(), Dir::new());
    let mut e = open(&io, &m, &d);
    for i in 0..RATE_PER_ROLE as u64 {
        e.handle(&sig("fuel", i, 1), 100);
        e.observe_online(&BTreeSet::from(["fuel".to_string()]), 100)
            .unwrap();
    }
    e.handle(&sig("fuel", 9, 1), 200); // refused: rate
    assert!(e.recheck(300).unwrap().is_empty(), "no slot has freed yet");
    let later = 100 + HOUR_SECS + 1;
    assert_eq!(e.recheck(later).unwrap(), vec!["fuel".to_string()]);
    // (q) the clear call failed: the row is still there and is offered again next cycle
    assert_eq!(e.recheck(later + 30).unwrap(), vec!["fuel".to_string()]);
    assert!(e
        .data()
        .requests
        .iter()
        .any(|r| r.status == Status::Clearing));
    e.cleared("fuel").unwrap();
    assert!(e
        .data()
        .requests
        .iter()
        .all(|r| r.status != Status::Clearing));
}

// (g) a signal carrying a model field launches with sonnet
#[test]
fn a_signal_with_a_model_field_launches_with_sonnet() {
    let s: Signal = serde_json::from_str(
        r#"{"role":"fuel","signal_id":"fuel:1","attempt":1,"epoch":"e","model":"claude-opus-4","token":"x"}"#,
    )
    .unwrap();
    let (io, m, d) = (Mem::new(), started(), Dir::new());
    let mut e = open(&io, &m, &d);
    e.handle(&s, 10);
    assert_eq!(m.calls.borrow()[0], ("fuel".into(), "sonnet".into()));
}

// (g2) a stored refusal is re-evaluated, not replayed
#[test]
fn a_stored_refusal_is_re_evaluated_not_replayed() {
    let (io, m, d) = (Mem::new(), started(), Dir::new());
    d.grant.set(false);
    let mut e = open(&io, &m, &d);
    let s = sig("fuel", 1, 1);
    assert_eq!(got(&e.handle(&s, 10)), res(Outcome::Refused, "no-grant"));
    d.grant.set(true);
    // the same key, inside the window: the old refusal is not imposed again
    assert_eq!(got(&e.handle(&s, 20)), res(Outcome::Started, "started"));
}

// (g3) invalid-argument stays refused on repeats
#[test]
fn invalid_argument_stays_refused() {
    let (io, d) = (Mem::new(), Dir::new());
    let m = Mech::new(vec![MechResult::InvalidArgument]);
    let mut e = open(&io, &m, &d);
    for t in [10, 20, 30] {
        assert_eq!(
            got(&e.handle(&sig("fuel", 1, 1), t)),
            res(Outcome::Refused, "invalid-argument")
        );
    }
}

// (h) a restart between two polls does not relaunch inside the window
#[test]
fn a_restart_does_not_relaunch_inside_the_window() {
    let (io, m, d) = (Mem::new(), started(), Dir::new());
    {
        let mut e = open(&io, &m, &d);
        e.handle(&sig("fuel", 5, 1), 1000);
    }
    let mut e2 = Engine::open(&io, &m, &d, false).unwrap();
    e2.set_allowlist_confirmed(true);
    let r = e2.handle(&sig("fuel", 5, 1), 1100);
    assert_eq!(got(&r), res(Outcome::Started, "started"));
    assert_eq!(m.n(), 1);
}

// (i) every mapping row of section 3
#[test]
fn every_mechanism_result_maps_to_a_named_outcome() {
    let cases = [
        (MechResult::Working, Outcome::Started, "started"),
        (MechResult::Started, Outcome::Started, "started"),
        (
            MechResult::AwaitingDialog,
            Outcome::Started,
            "awaiting-dialog",
        ),
        (
            MechResult::AlreadyRunning,
            Outcome::AlreadyRunning,
            "already-running",
        ),
        (
            MechResult::HeldForMemory,
            Outcome::Failed,
            "held-for-memory",
        ),
        (MechResult::LockHeld, Outcome::Failed, "lock-held"),
        (
            MechResult::Failed("boom".into()),
            Outcome::Failed,
            "spawn-failed",
        ),
        (
            MechResult::Failed("model-not-pinned".into()),
            Outcome::Failed,
            "model-not-pinned",
        ),
        (
            MechResult::InvalidArgument,
            Outcome::Refused,
            "invalid-argument",
        ),
        (MechResult::Timeout, Outcome::Failed, "mechanism-timeout"),
    ];
    for (i, (answer, o, why)) in cases.into_iter().enumerate() {
        let (io, d) = (Mem::new(), Dir::new());
        let m = Mech::new(vec![answer]);
        let mut e = open(&io, &m, &d);
        let r = e.handle(&sig("fuel", i as u64, 1), 10);
        assert_eq!(got(&r), res(o, why), "case {i}");
    }
}

// (l) an unreadable or unwritable store answers failed:store-unavailable and calls nothing
#[test]
fn an_unwritable_store_fails_closed_and_calls_nothing() {
    let (io, m, d) = (Mem::new(), started(), Dir::new());
    let mut e = open(&io, &m, &d);
    io.fail_save.set(true);
    let r = e.handle(&sig("fuel", 1, 1), 10);
    assert_eq!(got(&r), res(Outcome::Failed, "store-unavailable"));
    assert_eq!(m.n(), 0);
    assert!(e.data().requests.is_empty(), "no phantom row");
    // control: the same engine, store writable again, launches
    io.fail_save.set(false);
    assert_eq!(got(&e.handle(&sig("fuel", 1, 1), 20)).0, Outcome::Started);
}

#[test]
fn a_missing_or_unreadable_store_is_never_created_on_the_engines_own_initiative() {
    let (io, m, d) = (Mem::new(), started(), Dir::new());
    assert_eq!(
        Engine::open(&io, &m, &d, false).err(),
        Some(StoreError::Missing)
    );
    io.fail_load.set(true);
    assert!(matches!(
        Engine::open(&io, &m, &d, true).err(),
        Some(StoreError::Unreadable(_))
    ));
}

#[test]
fn a_newer_schema_is_refused_by_the_file_store_and_not_rewritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("launcher-store.json");
    std::fs::write(&path, br#"{"schema":99,"requests":[],"roles":{}}"#).unwrap();
    let io = FileStore { path: path.clone() };
    assert_eq!(io.load().err(), Some(StoreError::Newer(99)));
    assert_eq!(
        std::fs::read(&path).unwrap(),
        br#"{"schema":99,"requests":[],"roles":{}}"#.to_vec()
    );
    // control: our own schema round-trips
    let ok = FileStore {
        path: dir.path().join("ok.json"),
    };
    ok.save(&StoreData::default()).unwrap();
    assert_eq!(ok.load().unwrap(), Some(StoreData::default()));
}

// (m) three spawns with no online observation become refused:no-claim; seen online clears it
#[test]
fn three_spawns_without_a_claim_become_no_claim_and_being_seen_online_clears_it() {
    let (io, m, d) = (Mem::new(), started(), Dir::new());
    let mut e = open(&io, &m, &d);
    for i in 0..NO_CLAIM_BOUND as u64 {
        assert_eq!(
            got(&e.handle(&sig("fuel", i, 1), 10 + i as i64)).0,
            Outcome::Started
        );
    }
    assert_eq!(
        got(&e.handle(&sig("fuel", 50, 1), 100)),
        res(Outcome::Refused, "no-claim")
    );
    assert!(e.recheck(110).unwrap().is_empty(), "still no claim");
    e.observe_online(&BTreeSet::from(["fuel".to_string()]), 120)
        .unwrap();
    assert_eq!(e.recheck(130).unwrap(), vec!["fuel".to_string()]);
    assert_eq!(e.data().roles["fuel"].spawns_since_online, 0);
}

// (n) records older than 24 h are pruned
#[test]
fn records_older_than_a_day_are_pruned() {
    let (io, m, d) = (Mem::new(), started(), Dir::new());
    let mut e = open(&io, &m, &d);
    e.handle(&sig("fuel", 1, 1), 0);
    e.handle(&sig("other", 1, 1), PRUNE_SECS + 5);
    assert_eq!(e.data().requests.len(), 1);
    assert_eq!(e.data().requests[0].role, "other");
}

// (o) a late mechanism result is recorded and the next attempt answers already-running
#[test]
fn a_late_result_after_a_timeout_is_recorded_and_counted() {
    let (io, d) = (Mem::new(), Dir::new());
    let m = Mech::new(vec![MechResult::Timeout, MechResult::AlreadyRunning]);
    let mut e = open(&io, &m, &d);
    let s = sig("fuel", 1, 1);
    assert_eq!(
        got(&e.handle(&s, 10)),
        res(Outcome::Failed, "mechanism-timeout")
    );
    assert!(
        !e.data().requests[0].counts_spawn(),
        "uncounted until it arrives"
    );
    assert!(e.late_result(&s, &MechResult::Started, 20).unwrap());
    assert!(e.data().requests[0].counts_spawn());
    assert_eq!(e.data().roles["fuel"].spawns_since_online, 1);
    assert_eq!(
        got(&e.handle(&sig("fuel", 1, 2), 30)),
        res(Outcome::AlreadyRunning, "already-running")
    );
}

// (p) already-running and held-for-memory never consume the spawn budget
#[test]
fn already_running_and_held_for_memory_do_not_consume_the_budget() {
    let (io, d) = (Mem::new(), Dir::new());
    let m = Mech::new(vec![MechResult::AlreadyRunning, MechResult::HeldForMemory]);
    let mut e = open(&io, &m, &d);
    for i in 0..10u64 {
        e.handle(&sig("fuel", i, 1), 10 + i as i64);
    }
    assert_eq!(e.spawns_last_hour(100, None), 0);
    assert_eq!(e.data().roles["fuel"].spawns_since_online, 0);
}

// (r) launch, seen online, exit - three times in a day - and a fourth launch
#[test]
fn a_legitimate_cycle_never_trips_the_bounds() {
    let (io, m, d) = (Mem::new(), started(), Dir::new());
    let mut e = open(&io, &m, &d);
    // hours apart so the hourly cap is not the thing under test
    for (k, t) in [0i64, 7200, 14400, 21600].into_iter().enumerate() {
        assert_eq!(
            got(&e.handle(&sig("fuel", k as u64, 1), t)),
            res(Outcome::Started, "started"),
            "launch {k}"
        );
        e.observe_online(&BTreeSet::from(["fuel".to_string()]), t + 30)
            .unwrap();
    }
    assert_eq!(m.n(), 4);
}

// (s) an already-running streak with no online observation raises exactly one alarm
#[test]
fn an_already_running_streak_raises_one_alarm_until_the_role_is_seen_online() {
    let (io, d) = (Mem::new(), Dir::new());
    let m = Mech::new(vec![MechResult::Started, MechResult::AlreadyRunning]);
    let mut e = open(&io, &m, &d);
    e.handle(&sig("fuel", 0, 1), 10);
    for i in 1..=STALL_STREAK as u64 {
        e.handle(&sig("fuel", i, 1), 10 + i as i64);
    }
    let a = e.alarms(60, 120);
    assert_eq!(
        a,
        vec!["launch-stalled: fuel already-running-streak".to_string()]
    );
    assert!(e.alarms(61, 120).is_empty(), "once per streak");
    // (u) seen online: the alarm re-arms
    e.observe_online(&BTreeSet::from(["fuel".to_string()]), 70)
        .unwrap();
    assert!(!e.data().roles["fuel"].alarmed);
}

// (t) a started older than 2 x launch_wait, never seen online, raises it once
#[test]
fn a_stale_started_never_claimed_raises_the_alarm_once() {
    let (io, m, d) = (Mem::new(), started(), Dir::new());
    let mut e = open(&io, &m, &d);
    e.handle(&sig("fuel", 1, 1), 100);
    assert!(
        e.alarms(100 + 240, 120).is_empty(),
        "exactly 2 x launch_wait is not older"
    );
    assert_eq!(
        e.alarms(100 + 241, 120),
        vec!["launch-stalled: fuel started-never-claimed".to_string()]
    );
    assert!(e.alarms(100 + 500, 120).is_empty());
}

// A crash between the intent save and the answer: never called again.
#[test]
fn a_row_left_in_intent_by_a_crash_is_resolved_not_re_called() {
    let (io, m, d) = (Mem::new(), started(), Dir::new());
    // Simulate the crash: a store whose only row is Intent.
    let crashed = StoreData {
        schema: SCHEMA,
        requests: vec![Request {
            signal_id: "fuel:1".into(),
            attempt: 1,
            epoch: String::new(),
            generation: 0,
            role: "fuel".into(),
            at: 10,
            status: Status::Intent,
            outcome: Outcome::Failed,
            reason: "in-flight".into(),
            called: true,
            posted: false,
        }],
        roles: BTreeMap::new(),
    };
    io.save(&crashed).unwrap();
    d.running.set(true);
    let mut e = Engine::open(&io, &m, &d, false).unwrap();
    assert!(
        e.outbox().is_empty(),
        "an in-flight row is not postable yet"
    );
    assert_eq!(e.recover(20).unwrap(), 1);
    let out = e.outbox();
    assert_eq!(got(&out[0]), res(Outcome::Started, "started"));
    // the same signal again, inside the window: the recorded result, no mechanism call
    assert_eq!(
        got(&e.handle(&sig("fuel", 1, 1), 30)),
        res(Outcome::Started, "started")
    );
    assert_eq!(m.n(), 0);
}

#[test]
fn an_unacknowledged_result_stays_in_the_outbox_until_posted() {
    let (io, m, d) = (Mem::new(), started(), Dir::new());
    let mut e = open(&io, &m, &d);
    e.handle(&sig("fuel", 1, 1), 10);
    assert_eq!(e.outbox().len(), 1);
    assert_eq!(e.outbox().len(), 1, "reading it does not drain it");
    let r = e.outbox().remove(0);
    let mut other_epoch = r.clone();
    other_epoch.epoch = "other".into();
    e.mark_posted(&other_epoch).unwrap();
    assert_eq!(e.outbox().len(), 1, "another epoch acknowledges nothing");
    e.mark_posted(&r).unwrap();
    assert!(e.outbox().is_empty());
}

#[test]
fn a_rule_7_refusal_is_cleared_when_a_grant_appears() {
    let (io, m, d) = (Mem::new(), started(), Dir::new());
    d.grant.set(false);
    let mut e = open(&io, &m, &d);
    e.handle(&sig("fuel", 1, 1), 10);
    assert!(e.recheck(20).unwrap().is_empty());
    d.grant.set(true);
    assert_eq!(e.recheck(30).unwrap(), vec!["fuel".to_string()]);
}

// An alarm is only raised once it reached someone: until `mark_alarmed`, it is offered again.
#[test]
fn an_alarm_stays_pending_until_it_is_marked_delivered() {
    let (io, m, d) = (Mem::new(), started(), Dir::new());
    let mut e = open(&io, &m, &d);
    e.handle(&sig("fuel", 1, 1), 100);
    let due = e.pending_alarms(100 + 241, 120);
    assert_eq!(
        due,
        vec![(
            "fuel".to_string(),
            "launch-stalled: fuel started-never-claimed".to_string()
        )]
    );
    assert_eq!(
        e.pending_alarms(100 + 242, 120),
        due,
        "an undelivered alarm is offered again"
    );
    e.mark_alarmed("fuel").unwrap();
    assert!(e.pending_alarms(100 + 243, 120).is_empty());
}

// The `Clearing` mark must be durable before the caller clears: a save failure is reported, and
// the next recheck offers the role again.
#[test]
fn recheck_reports_a_save_failure_so_the_caller_does_not_clear() {
    let (io, m, d) = (Mem::new(), started(), Dir::new());
    let mut e = open(&io, &m, &d);
    for i in 0..RATE_PER_ROLE as u64 {
        e.handle(&sig("fuel", i, 1), 100);
        e.observe_online(&BTreeSet::from(["fuel".to_string()]), 100)
            .unwrap();
    }
    e.handle(&sig("fuel", 9, 1), 200); // refused: rate
    let later = 100 + HOUR_SECS + 1;
    io.fail_save.set(true);
    assert!(e.recheck(later).is_err(), "the Clearing mark did not save");
    io.fail_save.set(false);
    assert_eq!(e.recheck(later + 1).unwrap(), vec!["fuel".to_string()]);
}

// ---- rule 7b: a grant is not enough without a confirmed sender allowlist ----

#[test]
fn a_grant_without_a_confirmed_sender_allowlist_is_refused_and_the_mechanism_is_not_called() {
    let (io, m, d) = (Mem::new(), started(), Dir::new());
    let mut e = Engine::open(&io, &m, &d, true).unwrap();
    // The default is "not confirmed": a fresh engine has never been told.
    let r = e.handle(&sig("fuel", 1, 1), 1000);
    assert_eq!(got(&r), res(Outcome::Refused, "no-sender-allowlist"));
    assert_eq!(m.n(), 0);
    // Confirmed: the same lane launches (control: the refusal above was this rule and no other).
    e.set_allowlist_confirmed(true);
    let r = e.handle(&sig("fuel", 2, 1), 1100);
    assert_eq!(got(&r), res(Outcome::Started, "started"));
    assert_eq!(m.n(), 1);
    // And withdrawn again: refused again.
    e.set_allowlist_confirmed(false);
    let r = e.handle(&sig("fuel", 3, 1), 1200);
    assert_eq!(got(&r), res(Outcome::Refused, "no-sender-allowlist"));
    assert_eq!(m.n(), 1);
}

#[test]
fn with_no_grant_the_reason_is_still_no_grant_whatever_the_allowlist_says() {
    let (io, m, d) = (Mem::new(), started(), Dir::new());
    d.grant.set(false);
    let mut e = Engine::open(&io, &m, &d, true).unwrap();
    for (id, confirmed) in [(10, false), (11, true)] {
        e.set_allowlist_confirmed(confirmed);
        let r = e.handle(&sig("fuel", id, 1), 1000);
        assert_eq!(got(&r), res(Outcome::Refused, "no-grant"), "{confirmed}");
    }
}
