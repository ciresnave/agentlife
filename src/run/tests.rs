use super::*;
use crate::clock::ManualClock;
use crate::consent::fake::FakeConsent;
use crate::consent::{AnswerError, Asking, Grant, Kind, Outcome, Request, Requester, Resolved};
use crate::identity::SysinfoTable;
use crate::plan::{Entry, Params, PLAN_SCHEMA};
use crate::restore::{Summary, REPORT_SCHEMA};
use chrono::{DateTime, Duration, TimeZone, Utc};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 10, 12, 0, 0).unwrap()
}

fn agentlife() -> Requester {
    Requester {
        role: pending::ROLE.into(),
        session_id: String::new(),
        claude_pid: 4242,
        claude_start_secs: 1_700_000_000,
        managed: false,
    }
}

fn entry(id: &str) -> Entry {
    Entry {
        agent_id: id.into(),
        name: Some(id.into()),
        title: id.into(),
        cwd: format!("C:/Projects/{id}"),
        argv: vec!["--name".into(), id.into()],
        mode: Some("default".into()),
        pm: false,
        flags: vec![],
        dropped_args: vec![],
        batch: 1,
        window: 1,
        tab: 1,
    }
}

fn plan_with(ids: &[&str]) -> Plan {
    let mut p = Plan {
        schema: PLAN_SCHEMA,
        params: Params {
            batch_size: 3,
            batch_delay_secs: 30,
            liveness_timeout_secs: 60,
            tabs_per_window: 4,
            max_running: None,
            free_ram_floor_gb: 4.0,
        },
        running_now: 0,
        free_ram_gb: Some(32.0),
        entries: ids.iter().map(|i| entry(i)).collect(),
        held: vec![],
        excluded: vec![],
        hash: String::new(),
    };
    p.hash = p.compute_hash();
    p
}

fn report_for(plan: &Plan) -> Report {
    Report {
        schema: REPORT_SCHEMA,
        plan_hash: plan.hash.clone(),
        started_at: t0(),
        finished_at: t0(),
        halted: None,
        held_for_memory: false,
        preflight: vec![],
        entries: vec![],
        summary: Summary::default(),
    }
}

/// One fake store the runtime and the test's closures can both reach.
#[derive(Clone)]
struct Shared(Rc<RefCell<FakeConsent>>);

impl Consent for Shared {
    fn submit(&mut self, req: &Request, grant: &Grant, bound_hash: &str) -> Result<String, String> {
        self.0.borrow_mut().submit(req, grant, bound_hash)
    }
    fn begin_answer(&mut self, id: &str, bound_hash: &str) -> Result<Asking, AnswerError> {
        self.0.borrow_mut().begin_answer(id, bound_hash)
    }
    fn resolve(&mut self, asking: &Asking, outcome: Outcome) -> Result<Resolved, String> {
        self.0.borrow_mut().resolve(asking, outcome)
    }
    fn approved_at(
        &mut self,
        kind: Kind,
        subject: &str,
        requester: &Requester,
    ) -> Result<DateTime<Utc>, String> {
        self.0.borrow_mut().approved_at(kind, subject, requester)
    }
    fn spend_one_use(
        &mut self,
        kind: Kind,
        subject: &str,
        requester: &Requester,
    ) -> Result<String, String> {
        self.0.borrow_mut().spend_one_use(kind, subject, requester)
    }
    fn withdraw(&mut self, id: &str) -> Result<bool, String> {
        self.0.borrow_mut().withdraw(id)
    }
    fn pending_ids(&self) -> Result<Vec<String>, String> {
        self.0.borrow().pending_ids()
    }
}

/// Answers as told; records into the shared event log that it was asked.
struct Scripted {
    outcome: Outcome,
    asked: Cell<u32>,
    events: Rc<RefCell<Vec<String>>>,
    /// Runs while the person "decides".
    during: Box<dyn Fn()>,
}

impl Prompt for Scripted {
    fn ask(&self, _: &Asking) -> Outcome {
        self.asked.set(self.asked.get() + 1);
        self.events.borrow_mut().push("ask".into());
        (self.during)();
        self.outcome
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    home: Home,
    clock: Arc<ManualClock>,
    consent: Shared,
    events: Rc<RefCell<Vec<String>>>,
    executed: Rc<RefCell<Vec<String>>>,
    me: ProcessIdentity,
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path()).unwrap();
    let clock = Arc::new(ManualClock::new(t0()));
    let consent = Shared(Rc::new(RefCell::new(FakeConsent::new(clock.clone()))));
    let me = SysinfoTable
        .identity_of(std::process::id())
        .expect("this test process");
    Rig {
        _dir: dir,
        home,
        clock,
        consent,
        events: Rc::default(),
        executed: Rc::default(),
        me,
    }
}

impl Rig {
    fn prompt(&self, outcome: Outcome, during: impl Fn() + 'static) -> Scripted {
        Scripted {
            outcome,
            asked: Cell::new(0),
            events: self.events.clone(),
            during: Box::new(during),
        }
    }

    /// Runs `f` with a runtime whose `rebuild` returns whatever `now` holds.
    fn with<T>(
        &mut self,
        caller: Caller,
        prompt: &Scripted,
        now: &RefCell<Plan>,
        f: impl FnOnce(&mut Runtime) -> T,
    ) -> T {
        let events = self.events.clone();
        let show = move |text: &str| {
            assert!(!text.is_empty());
            events.borrow_mut().push("show".into());
        };
        let witness = self.consent.clone();
        let executed = self.executed.clone();
        let events2 = self.events.clone();
        let req = agentlife();
        let execute = move |p: &Plan| {
            events2.borrow_mut().push("execute".into());
            // By the time anything runs, the approval is gone from the store.
            assert!(
                witness
                    .clone()
                    .approved_at(
                        Kind::RestorePlan,
                        &consent::restore_plan_subject(&p.hash).unwrap(),
                        &req
                    )
                    .is_err(),
                "execute ran with the approval still unspent"
            );
            executed.borrow_mut().push(p.hash.clone());
            report_for(p)
        };
        let rebuild = || Ok(now.borrow().clone());
        let table = SysinfoTable;
        let requester = agentlife();
        let mut consent = self.consent.clone();
        let mut rt = Runtime {
            home: &self.home,
            consent: &mut consent,
            prompt,
            requester: &requester,
            caller: &caller,
            clock: &*self.clock,
            table: &table,
            me: &self.me,
            show: &show,
            rebuild: &rebuild,
            execute: &execute,
        };
        f(&mut rt)
    }
}

use crate::consent;

#[test]
fn a_person_approves_the_list_is_shown_first_then_spent_then_run_then_reported() {
    let mut r = rig();
    let p = plan_with(&["a", "b"]);
    let now = RefCell::new(p.clone());
    let prompt = r.prompt(Outcome::Approved, || {});
    let ran = r
        .with(Caller::Person, &prompt, &now, |rt| {
            restore_now(rt, &p, "reboot")
        })
        .unwrap();
    let Ran::Executed {
        report,
        report_path,
        ..
    } = ran
    else {
        panic!("expected a run, got {ran:?}")
    };
    assert_eq!(report.plan_hash, p.hash);
    assert!(report_path.unwrap().is_file(), "the report is on disk");
    assert_eq!(*r.events.borrow(), ["show", "ask", "execute"]);
    assert_eq!(
        r.executed.borrow().as_slice(),
        std::slice::from_ref(&p.hash)
    );
}

#[test]
fn an_agent_or_an_unclear_caller_is_refused_before_anything_is_stored_shown_or_asked() {
    for caller in [
        Caller::Agent { id: None },
        Caller::Unclear("a node in the way".into()),
    ] {
        let mut r = rig();
        let p = plan_with(&["a"]);
        let now = RefCell::new(p.clone());
        let prompt = r.prompt(Outcome::Approved, || {});
        let err = r
            .with(caller, &prompt, &now, |rt| restore_now(rt, &p, "reboot"))
            .unwrap_err();
        assert!(err.starts_with("refused:"), "{err}");
        assert!(r.events.borrow().is_empty(), "{:?}", r.events.borrow());
        assert!(pending::list(&r.home).0.is_empty(), "no record was made");
        assert_eq!(r.consent.0.borrow().prompts_begun, 0);
        assert!(r.consent.pending_ids().unwrap().is_empty());
        // Positive control: the same call as a person goes through.
        let prompt = r.prompt(Outcome::Approved, || {});
        let ran = r
            .with(Caller::Person, &prompt, &now, |rt| {
                restore_now(rt, &p, "reboot")
            })
            .unwrap();
        assert!(matches!(ran, Ran::Executed { .. }));
    }
}

#[test]
fn an_agent_is_refused_on_pending_approve_too() {
    let mut r = rig();
    let p = plan_with(&["a"]);
    let rec = pending::create(&r.home, &mut r.consent, &p, "x", t0()).unwrap();
    let now = RefCell::new(p.clone());
    let prompt = r.prompt(Outcome::Approved, || {});
    let err = r
        .with(Caller::Agent { id: None }, &prompt, &now, |rt| {
            approve_pending(rt, &rec.pending_id)
        })
        .unwrap_err();
    assert!(err.starts_with("refused:"), "{err}");
    assert_eq!(prompt.asked.get(), 0);
    assert!(r.executed.borrow().is_empty());
}

#[test]
fn a_cancel_starts_nothing_and_closes_the_request() {
    let mut r = rig();
    let p = plan_with(&["a"]);
    let now = RefCell::new(p.clone());
    let prompt = r.prompt(Outcome::Cancelled, || {});
    let ran = r
        .with(Caller::Person, &prompt, &now, |rt| restore_now(rt, &p, "x"))
        .unwrap();
    assert!(matches!(ran, Ran::Cancelled { .. }), "{ran:?}");
    assert!(r.executed.borrow().is_empty());
}

#[test]
fn a_timeout_or_an_unavailable_channel_starts_nothing_and_leaves_it_pending() {
    for outcome in [Outcome::TimedOut, Outcome::Unavailable] {
        let mut r = rig();
        let p = plan_with(&["a"]);
        let now = RefCell::new(p.clone());
        let prompt = r.prompt(outcome, || {});
        let ran = r
            .with(Caller::Person, &prompt, &now, |rt| restore_now(rt, &p, "x"))
            .unwrap();
        assert!(matches!(ran, Ran::StillPending { .. }), "{ran:?}");
        assert!(r.executed.borrow().is_empty());
        assert!(pending::list(&r.home).0[0].is_open());
    }
}

#[test]
fn a_plan_that_changed_while_the_person_decided_is_not_spent_and_not_run() {
    let mut r = rig();
    let p = plan_with(&["a", "b"]);
    let now = Rc::new(RefCell::new(p.clone()));
    // While the prompt is up, "b" starts by itself: the plan built now is a different plan.
    let during_now = now.clone();
    let prompt = r.prompt(Outcome::Approved, move || {
        *during_now.borrow_mut() = plan_with(&["a"]);
    });
    let err = r
        .with(Caller::Person, &prompt, &now, |rt| restore_now(rt, &p, "x"))
        .unwrap_err();
    assert!(
        err.contains("nothing was spent, nothing was started"),
        "{err}"
    );
    assert!(r.executed.borrow().is_empty());
    // Truthful: the approval really is unspent (a positive control that the read works).
    let subject = consent::restore_plan_subject(&p.hash).unwrap();
    assert!(r
        .consent
        .clone()
        .approved_at(Kind::RestorePlan, &subject, &agentlife())
        .is_ok());
}

#[test]
fn approving_a_request_whose_plan_no_longer_matches_voids_it_unasked() {
    let mut r = rig();
    let p = plan_with(&["a", "b"]);
    let rec = pending::create(&r.home, &mut r.consent, &p, "x", t0()).unwrap();
    let now = RefCell::new(plan_with(&["a"]));
    let prompt = r.prompt(Outcome::Approved, || {});
    let ran = r
        .with(Caller::Person, &prompt, &now, |rt| {
            approve_pending(rt, &rec.pending_id)
        })
        .unwrap();
    assert!(matches!(ran, Ran::Voided { .. }), "{ran:?}");
    assert_eq!(prompt.asked.get(), 0, "nobody was asked");
    assert!(r.executed.borrow().is_empty());
}

#[test]
fn an_approval_older_than_the_window_is_spent_and_refused_and_nothing_runs() {
    let r = rig();
    let p = plan_with(&["a"]);
    let now = RefCell::new(p.clone());
    let prompt = r.prompt(Outcome::Approved, || {});
    // The person answers, then the machine is slow: `rebuild` runs between the answer and the spend,
    // and that is where the time passes.
    let clock2 = r.clock.clone();
    let ran = {
        let events = r.events.clone();
        let show = move |_: &str| events.borrow_mut().push("show".into());
        let rebuild = || {
            clock2.set(t0() + Duration::seconds(pending::APPROVAL_FRESH_SECS + 60));
            Ok(now.borrow().clone())
        };
        let executed = r.executed.clone();
        let execute = move |p: &Plan| {
            executed.borrow_mut().push(p.hash.clone());
            report_for(p)
        };
        let table = SysinfoTable;
        let requester = agentlife();
        let mut consent = r.consent.clone();
        let caller = Caller::Person;
        let mut rt = Runtime {
            home: &r.home,
            consent: &mut consent,
            prompt: &prompt,
            requester: &requester,
            caller: &caller,
            clock: &*r.clock,
            table: &table,
            me: &r.me,
            show: &show,
            rebuild: &rebuild,
            execute: &execute,
        };
        restore_now(&mut rt, &p, "x")
    };
    let err = ran.unwrap_err();
    assert!(err.contains("spent and refused"), "{err}");
    assert!(r.executed.borrow().is_empty());
}

#[test]
fn a_second_restore_while_one_holds_the_lock_asks_nobody() {
    let mut r = rig();
    let p = plan_with(&["a"]);
    let now = RefCell::new(p.clone());
    let _held = RestoreLock::acquire(&r.home, &r.me, &SysinfoTable).unwrap();
    let prompt = r.prompt(Outcome::Approved, || {});
    let err = r
        .with(Caller::Person, &prompt, &now, |rt| restore_now(rt, &p, "x"))
        .unwrap_err();
    assert!(err.contains("another restore is running"), "{err}");
    assert_eq!(prompt.asked.get(), 0);
    assert!(r.executed.borrow().is_empty());
}

#[test]
fn a_handoff_is_found_by_its_plain_name_and_never_by_a_path_like_one() {
    let d = tempfile::tempdir().unwrap();
    let mut e = entry("lane");
    e.cwd = d.path().display().to_string();
    assert!(!entry_has_handoff(&e), "nothing there yet");
    std::fs::write(d.path().join("LANE-HANDOFF.md"), "x").unwrap();
    assert!(entry_has_handoff(&e));
    e.name = Some("../lane".into());
    assert!(!entry_has_handoff(&e));
}

#[test]
fn a_plan_that_starts_nothing_is_not_asked_about() {
    let mut r = rig();
    let p = plan_with(&[]);
    let now = RefCell::new(p.clone());
    let prompt = r.prompt(Outcome::Approved, || {});
    let ran = r
        .with(Caller::Person, &prompt, &now, |rt| restore_now(rt, &p, "x"))
        .unwrap();
    assert!(matches!(ran, Ran::NothingToStart));
    assert_eq!(prompt.asked.get(), 0);
}

#[test]
fn a_second_run_cannot_reuse_the_spent_approval() {
    let mut r = rig();
    let p = plan_with(&["a"]);
    let now = RefCell::new(p.clone());
    let prompt = r.prompt(Outcome::Approved, || {});
    r.with(Caller::Person, &prompt, &now, |rt| restore_now(rt, &p, "x"))
        .unwrap();
    // Straight to the spend: the record is closed Approved but the approval is gone.
    let rec = pending::list(&r.home).0.remove(0);
    let err = pending::spend_approval(&r.home, &mut r.consent, &agentlife(), &rec.pending_id, t0())
        .unwrap_err();
    assert!(err.contains("not spending"), "{err}");
}
