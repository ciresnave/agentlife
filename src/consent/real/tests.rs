use super::*;
use crate::consent::{answer, restore_plan_subject};
use std::cell::Cell;
use std::sync::Mutex;

/// A key protector that works on both CI legs. The production one is DPAPI, which the crate refuses
/// off Windows; what is under test here is the mapping onto the store, not DPAPI.
struct XorProtector;

impl Protector for XorProtector {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, String> {
        Ok(plain.iter().map(|b| b ^ 0x5a).collect())
    }
    fn unprotect(&self, blob: &[u8]) -> Result<Vec<u8>, String> {
        self.protect(blob)
    }
}

fn me() -> Requester {
    Requester {
        role: "agentlife".into(),
        session_id: String::new(),
        claude_pid: 4242,
        claude_start_secs: 1_700_000_000,
        managed: false,
    }
}

fn backend_in(dir: &std::path::Path, requester: Requester) -> UserRequestBackend {
    UserRequestBackend::with_parts(
        dir.join("store"),
        dir.join("head").join("audit.head"),
        Box::new(XorProtector),
        requester,
    )
}

struct Rig {
    dir: tempfile::TempDir,
    backend: UserRequestBackend,
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let backend = backend_in(dir.path(), me());
    Rig { dir, backend }
}

fn hash(c: char) -> String {
    c.to_string().repeat(64)
}

fn request_for(h: &str) -> Request {
    Request {
        kind: Kind::RestorePlan,
        role: "agentlife".into(),
        subject: restore_plan_subject(h).unwrap(),
        summary: "restore 2 agents".into(),
        reason: "after the update".into(),
    }
}

fn submit(b: &mut UserRequestBackend, h: &str) -> String {
    b.submit(&request_for(h), &Grant::OneUse, h).unwrap()
}

/// Answers with what it is told and counts how often it was asked.
struct Scripted {
    outcome: Outcome,
    asked: Cell<u32>,
}

impl Scripted {
    fn new(outcome: Outcome) -> Self {
        Self {
            outcome,
            asked: Cell::new(0),
        }
    }
}

impl Prompt for Scripted {
    fn ask(&self, _: &Asking) -> Outcome {
        self.asked.set(self.asked.get() + 1);
        self.outcome
    }
}

fn approve(b: &mut UserRequestBackend, id: &str, h: &str) {
    let p = Scripted::new(Outcome::Approved);
    assert_eq!(answer(b, &p, id, h).unwrap(), Resolved::Granted);
    assert_eq!(p.asked.get(), 1);
}

#[test]
fn a_request_records_nothing_that_can_be_spent_until_a_person_approves_it() {
    let mut r = rig();
    let h = hash('a');
    let subject = restore_plan_subject(&h).unwrap();
    let id = submit(&mut r.backend, &h);
    assert_eq!(r.backend.pending_ids().unwrap(), vec![id.clone()]);
    let e = r
        .backend
        .spend_one_use(Kind::RestorePlan, &subject, &me())
        .unwrap_err();
    assert!(e.contains("no unspent approval"), "{e}");
    assert!(r
        .backend
        .approved_at(Kind::RestorePlan, &subject, &me())
        .is_err());
}

#[test]
fn the_same_request_again_is_the_same_id() {
    let mut r = rig();
    let h = hash('a');
    assert_eq!(submit(&mut r.backend, &h), submit(&mut r.backend, &h));
    assert_eq!(r.backend.pending_ids().unwrap().len(), 1);
}

#[test]
fn an_approval_is_found_read_before_the_spend_and_spent_exactly_once() {
    let mut r = rig();
    let h = hash('b');
    let subject = restore_plan_subject(&h).unwrap();
    let id = submit(&mut r.backend, &h);
    approve(&mut r.backend, &id, &h);
    assert!(
        r.backend.pending_ids().unwrap().is_empty(),
        "an answer spends its request"
    );

    let at = r
        .backend
        .approved_at(Kind::RestorePlan, &subject, &me())
        .unwrap();
    assert!((Utc::now() - at).num_seconds().abs() < 60, "{at}");
    // Reading it spends nothing.
    assert!(r
        .backend
        .approved_at(Kind::RestorePlan, &subject, &me())
        .is_ok());

    let spent = r
        .backend
        .spend_one_use(Kind::RestorePlan, &subject, &me())
        .unwrap();
    assert!(!spent.is_empty());
    let again = r
        .backend
        .spend_one_use(Kind::RestorePlan, &subject, &me())
        .unwrap_err();
    assert!(again.contains("no unspent approval"), "{again}");
    assert!(r
        .backend
        .approved_at(Kind::RestorePlan, &subject, &me())
        .is_err());
}

#[test]
fn an_approval_belongs_to_its_plan_and_a_later_process_can_spend_it() {
    let mut r = rig();
    let (a, b) = (hash('a'), hash('b'));
    let id = submit(&mut r.backend, &a);
    approve(&mut r.backend, &id, &a);

    // Another plan's subject finds nothing, and spends nothing.
    let other = restore_plan_subject(&b).unwrap();
    assert!(r
        .backend
        .spend_one_use(Kind::RestorePlan, &other, &me())
        .is_err());

    // A restore after a reboot is a new process (RestorePlan is Scope::AnyRequester).
    let later = Requester {
        claude_pid: 9999,
        claude_start_secs: 1_800_000_000,
        ..me()
    };
    let mut second = backend_in(r.dir.path(), later.clone());
    let subject = restore_plan_subject(&a).unwrap();
    second
        .spend_one_use(Kind::RestorePlan, &subject, &later)
        .unwrap();
    assert!(r
        .backend
        .spend_one_use(Kind::RestorePlan, &subject, &me())
        .is_err());
}

#[test]
fn a_plan_that_changed_voids_the_request_and_nobody_is_asked() {
    let mut r = rig();
    let id = submit(&mut r.backend, &hash('a'));
    let p = Scripted::new(Outcome::Approved);
    let e = answer(&mut r.backend, &p, &id, &hash('b')).unwrap_err();
    assert_eq!(e, AnswerError::Voided(Voided::Stale));
    assert_eq!(p.asked.get(), 0, "voided unasked");
    assert!(r.backend.pending_ids().unwrap().is_empty());
    // And nothing was granted.
    let subject = restore_plan_subject(&hash('a')).unwrap();
    assert!(r
        .backend
        .spend_one_use(Kind::RestorePlan, &subject, &me())
        .is_err());
}

#[test]
fn cancel_closes_the_request_and_grants_nothing() {
    let mut r = rig();
    let h = hash('c');
    let id = submit(&mut r.backend, &h);
    let p = Scripted::new(Outcome::Cancelled);
    assert_eq!(
        answer(&mut r.backend, &p, &id, &h).unwrap(),
        Resolved::Closed
    );
    assert!(r.backend.pending_ids().unwrap().is_empty());
    let subject = restore_plan_subject(&h).unwrap();
    assert!(r
        .backend
        .spend_one_use(Kind::RestorePlan, &subject, &me())
        .is_err());
}

#[test]
fn a_time_out_leaves_it_pending_and_asking_again_is_held_in_the_cooldown() {
    let mut r = rig();
    let h = hash('d');
    let id = submit(&mut r.backend, &h);
    let p = Scripted::new(Outcome::TimedOut);
    assert_eq!(
        answer(&mut r.backend, &p, &id, &h).unwrap(),
        Resolved::StillPending
    );
    assert_eq!(r.backend.pending_ids().unwrap(), vec![id.clone()]);
    let e = answer(&mut r.backend, &p, &id, &h).unwrap_err();
    assert!(matches!(e, AnswerError::Gate(_)), "{e:?}");
    assert_eq!(p.asked.get(), 1, "the person was not asked a second time");
    assert_eq!(r.backend.pending_ids().unwrap(), vec![id]);
}

#[test]
fn an_unavailable_channel_leaves_it_pending_and_can_be_asked_again() {
    let mut r = rig();
    let h = hash('e');
    let id = submit(&mut r.backend, &h);
    let p = Scripted::new(Outcome::Unavailable);
    assert_eq!(
        answer(&mut r.backend, &p, &id, &h).unwrap(),
        Resolved::StillPending
    );
    assert_eq!(r.backend.pending_ids().unwrap(), vec![id.clone()]);
    approve(&mut r.backend, &id, &h);
}

#[test]
fn withdrawing_ends_the_request_and_a_prompt_already_up_can_no_longer_be_approved() {
    let mut r = rig();
    let h = hash('f');
    let id = submit(&mut r.backend, &h);
    let asking = r.backend.begin_answer(&id, &h).unwrap();
    assert!(r.backend.withdraw(&id).unwrap());
    assert!(
        !r.backend.withdraw(&id).unwrap(),
        "false when there is none"
    );
    assert!(r.backend.resolve(&asking, Outcome::Approved).is_err());
    let subject = restore_plan_subject(&h).unwrap();
    assert!(r
        .backend
        .spend_one_use(Kind::RestorePlan, &subject, &me())
        .is_err());
}

#[test]
fn a_second_prompt_for_the_same_request_is_refused_while_one_is_up() {
    let mut r = rig();
    let h = hash('1');
    let id = submit(&mut r.backend, &h);
    let first = r.backend.begin_answer(&id, &h).unwrap();
    assert_eq!(
        r.backend.begin_answer(&id, &h).unwrap_err(),
        AnswerError::AlreadyAnswering
    );
    // The first can still be answered.
    assert_eq!(
        r.backend.resolve(&first, Outcome::Approved).unwrap(),
        Resolved::Granted
    );
}

#[test]
fn the_store_is_not_held_between_calls_so_it_is_free_while_a_prompt_is_up() {
    let mut r = rig();
    let h = hash('2');
    let id = submit(&mut r.backend, &h);
    let _asking = r.backend.begin_answer(&id, &h).unwrap();
    // A held store.lock would make this wait LOCK_WAIT (15 s) and fail.
    let t = std::time::Instant::now();
    let other = backend_in(r.dir.path(), me());
    assert_eq!(other.pending_ids().unwrap(), vec![id]);
    assert!(
        t.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        t.elapsed()
    );
}

#[test]
fn with_no_store_nothing_is_created_by_asking_and_nothing_is_found() {
    let mut r = rig();
    let store = r.dir.path().join("store");
    let h = hash('3');
    let subject = restore_plan_subject(&h).unwrap();
    assert!(r.backend.pending_ids().unwrap().is_empty());
    assert_eq!(
        r.backend.begin_answer("nope", &h).unwrap_err(),
        AnswerError::NoSuchRequest
    );
    assert!(!r.backend.withdraw("nope").unwrap());
    assert!(r
        .backend
        .approved_at(Kind::RestorePlan, &subject, &me())
        .is_err());
    assert!(r
        .backend
        .spend_one_use(Kind::RestorePlan, &subject, &me())
        .is_err());
    assert!(!store.exists(), "reading and spending never create a store");
    // Positive control: a submit does.
    submit(&mut r.backend, &h);
    assert!(store.exists());
}

#[test]
fn a_request_for_another_role_or_another_subject_is_refused_and_stores_nothing() {
    let mut r = rig();
    let h = hash('4');
    let mut other_role = request_for(&h);
    other_role.role = "overmind".into();
    assert!(r
        .backend
        .submit(&other_role, &Grant::OneUse, &h)
        .unwrap_err()
        .contains("asks as 'agentlife'"));

    let mut other_subject = request_for(&h);
    other_subject.subject = restore_plan_subject(&hash('5')).unwrap();
    assert!(r
        .backend
        .submit(&other_subject, &Grant::OneUse, &h)
        .is_err());

    let mut freeform = request_for(&h);
    freeform.subject = "restore 2 agents".into();
    assert!(r.backend.submit(&freeform, &Grant::OneUse, &h).is_err());

    // A timed grant is over a one-use kind's maximum: refused, never clamped.
    assert!(r
        .backend
        .submit(&request_for(&h), &Grant::For { secs: 1800 }, &h)
        .is_err());
    assert!(r.backend.pending_ids().unwrap().is_empty());
}

#[test]
fn what_the_person_is_shown_names_agentlife_as_not_a_registered_lane() {
    let mut r = rig();
    let h = hash('6');
    let id = submit(&mut r.backend, &h);
    let asking = r.backend.begin_answer(&id, &h).unwrap();
    assert_eq!(asking.request.role, "agentlife");
    assert_eq!(asking.grant, Grant::OneUse);
    let shown = ur::prompt_text(
        &to_ur_request(&asking.request, &me()),
        &to_ur_grant(&asking.grant),
        Utc::now(),
    );
    assert!(
        shown.contains("agentlife (pid 4242) - NOT a registered lane"),
        "{shown}"
    );
    assert!(shown.contains(&format!("plan {h}")), "{shown}");
}

#[test]
fn this_process_is_an_unregistered_requester_with_its_own_pid_and_start() {
    let me = this_process().unwrap();
    assert_eq!(me.role, "agentlife");
    assert!(me.session_id.is_empty());
    assert!(!me.managed);
    assert_eq!(me.claude_pid, std::process::id());
    assert!(me.claude_start_secs > 0);
}

#[test]
fn the_channels_answer_maps_without_ever_inventing_an_approval() {
    let approval = ur::Approval {
        kind: ur::KindId::RestorePlan,
        subject: "plan x".into(),
        requester: to_ur_requester(&me()),
        approved_at: Utc::now(),
        expires_at: None,
    };
    assert_eq!(
        outcome_from_channel(&ur::Outcome::Approved(approval)),
        Outcome::Approved
    );
    assert_eq!(
        outcome_from_channel(&ur::Outcome::Denied),
        Outcome::Cancelled
    );
    assert_eq!(
        outcome_from_channel(&ur::Outcome::TimedOut),
        Outcome::TimedOut
    );
    assert_eq!(
        outcome_from_channel(&ur::Outcome::Unavailable("no Hello".into())),
        Outcome::Unavailable
    );
    // Refused (over the maximum, malformed, ended while deciding) was not an answer: it leaves the
    // request pending rather than closing it as a denial.
    assert_eq!(
        outcome_from_channel(&ur::Outcome::Refused("over the maximum".into())),
        Outcome::Unavailable
    );
}

#[test]
fn a_begin_error_is_read_from_what_became_of_the_record() {
    use AnswerError::{AlreadyAnswering, Gate, Unavailable};
    let c = classify_begin_error;
    // Gone: voided, and the message only says which kind.
    assert_eq!(
        c(
            "was altered after it was made; voided",
            "a",
            "a",
            false,
            true
        ),
        AnswerError::Voided(Voided::Altered)
    );
    assert_eq!(
        c("x", "a", "b", false, true),
        AnswerError::Voided(Voided::Stale)
    );
    assert_eq!(
        c("x", "a", "a", false, true),
        AnswerError::Voided(Voided::Ended)
    );
    // Still there: the person may be asked later.
    assert_eq!(
        c("is already being answered", "a", "a", true, true),
        AlreadyAnswering
    );
    assert!(matches!(c("cooling", "a", "a", true, true), Gate(_)));
    assert!(matches!(
        c("untrusted", "a", "a", true, false),
        Unavailable(_)
    ));
}

#[test]
fn a_store_that_cannot_be_unprotected_fails_closed_everywhere() {
    struct Refuses;
    impl Protector for Refuses {
        fn protect(&self, _: &[u8]) -> Result<Vec<u8>, String> {
            Err("DPAPI is Windows-only".into())
        }
        fn unprotect(&self, _: &[u8]) -> Result<Vec<u8>, String> {
            Err("DPAPI is Windows-only".into())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let mut b = UserRequestBackend::with_parts(
        dir.path().join("store"),
        dir.path().join("head"),
        Box::new(Refuses),
        me(),
    );
    let h = hash('7');
    assert!(b
        .submit(&request_for(&h), &Grant::OneUse, &h)
        .unwrap_err()
        .contains("DPAPI"));
    let subject = restore_plan_subject(&h).unwrap();
    assert!(b.spend_one_use(Kind::RestorePlan, &subject, &me()).is_err());
    assert!(b.pending_ids().unwrap().is_empty());
}

/// Process-global environment: one test touches it, under this lock.
static ENV: Mutex<()> = Mutex::new(());

/// A test binary that sets the variable can never put a Hello dialog up, and what it gets is
/// `Unavailable`, which leaves the request pending (never an approval).
#[test]
fn hello_is_never_shown_when_the_debug_switch_is_set_and_the_answer_is_unavailable() {
    if !cfg!(debug_assertions) {
        return;
    }
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var(super::NO_HELLO_ENV, "1");
    let asking = Asking {
        pending_id: "p1".into(),
        request: request_for(&hash('1')),
        grant: Grant::OneUse,
        bound_hash: hash('1'),
        reservation: "r".into(),
    };
    let out = HelloPrompt::new(me()).ask(&asking);
    std::env::remove_var(super::NO_HELLO_ENV);
    assert_eq!(out, Outcome::Unavailable);
}

/// The production constructor, through the crate's own `locate` (debug overrides). On Windows the
/// key is really DPAPI-protected; elsewhere the crate refuses DPAPI and the backend fails closed.
#[test]
fn the_production_backend_uses_the_shared_stores_place_and_dpapi() {
    if !cfg!(debug_assertions) {
        return; // the overrides exist only in debug builds
    }
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("user-request");
    let head = dir.path().join("head").join("user-request-audit.head");
    std::env::set_var("USER_REQUEST_DIR", &store);
    std::env::set_var("USER_REQUEST_HEAD", &head);
    let made = UserRequestBackend::production(me());
    let result = made.and_then(|mut b| {
        let h = hash('9');
        let id = b.submit(&request_for(&h), &Grant::OneUse, &h)?;
        assert_eq!(b.pending_ids()?, vec![id]);
        Ok(())
    });
    std::env::remove_var("USER_REQUEST_DIR");
    std::env::remove_var("USER_REQUEST_HEAD");
    if cfg!(windows) {
        result.unwrap();
        for f in ["store.key", "store.key.check", "grants.json", "audit.jsonl"] {
            assert!(
                store.join(f).exists(),
                "{f} in the shared store's directory"
            );
        }
    } else {
        assert!(result.unwrap_err().contains("DPAPI"));
    }
}

mod through_pending {
    use super::*;
    use crate::home::Home;
    use crate::pending::{self, Answered};
    use crate::plan::{Entry, Params, Plan, PLAN_SCHEMA};

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

    fn plan_of(ids: &[&str]) -> Plan {
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

    struct World {
        home: Home,
        r: Rig,
        _home_dir: tempfile::TempDir,
    }

    fn world() -> World {
        let home_dir = tempfile::tempdir().unwrap();
        World {
            home: Home::new(home_dir.path()).unwrap(),
            r: rig(),
            _home_dir: home_dir,
        }
    }

    #[test]
    fn a_restore_that_was_never_approved_spends_nothing_and_returns_no_plan() {
        let mut w = world();
        let p = plan_of(&["a"]);
        let rec = pending::create(&w.home, &mut w.r.backend, &p, "x", Utc::now()).unwrap();
        let e = pending::spend_approval(
            &w.home,
            &mut w.r.backend,
            &me(),
            &rec.pending_id,
            Utc::now(),
        )
        .unwrap_err();
        assert!(e.contains("has not been approved"), "{e}");
        // Even a closed-as-approved record with no approval in the store yields no plan.
        let subject = restore_plan_subject(&p.hash).unwrap();
        assert!(w
            .r
            .backend
            .spend_one_use(Kind::RestorePlan, &subject, &me())
            .is_err());
    }

    #[test]
    fn approve_then_spend_runs_once_and_a_second_run_gets_nothing() {
        let mut w = world();
        let p = plan_of(&["a", "b"]);
        let rec = pending::create(&w.home, &mut w.r.backend, &p, "x", Utc::now()).unwrap();
        let prompt = Scripted::new(Outcome::Approved);
        let got = pending::answer(
            &w.home,
            &mut w.r.backend,
            &prompt,
            &rec.pending_id,
            &p,
            Utc::now(),
        )
        .unwrap();
        assert!(matches!(got, Answered::Approved(_)), "{got:?}");
        let frozen = pending::spend_approval(
            &w.home,
            &mut w.r.backend,
            &me(),
            &rec.pending_id,
            Utc::now(),
        )
        .unwrap();
        assert_eq!(frozen.plan.hash, p.hash);
        let again = pending::spend_approval(
            &w.home,
            &mut w.r.backend,
            &me(),
            &rec.pending_id,
            Utc::now(),
        )
        .unwrap_err();
        assert!(again.contains("not spending"), "{again}");
    }

    #[test]
    fn an_approval_older_than_five_minutes_is_spent_and_refused() {
        let mut w = world();
        let p = plan_of(&["a"]);
        let rec = pending::create(&w.home, &mut w.r.backend, &p, "x", Utc::now()).unwrap();
        let prompt = Scripted::new(Outcome::Approved);
        pending::answer(
            &w.home,
            &mut w.r.backend,
            &prompt,
            &rec.pending_id,
            &p,
            Utc::now(),
        )
        .unwrap();
        let later = Utc::now() + chrono::Duration::seconds(pending::APPROVAL_FRESH_SECS + 30);
        let e = pending::spend_approval(&w.home, &mut w.r.backend, &me(), &rec.pending_id, later)
            .unwrap_err();
        assert!(e.contains("freshness"), "{e}");
        let subject = restore_plan_subject(&p.hash).unwrap();
        assert!(
            w.r.backend
                .spend_one_use(Kind::RestorePlan, &subject, &me())
                .is_err(),
            "the stale approval was spent all the same"
        );
    }

    #[test]
    fn discard_withdraws_from_the_real_store() {
        let mut w = world();
        let p = plan_of(&["a"]);
        let rec = pending::create(&w.home, &mut w.r.backend, &p, "x", Utc::now()).unwrap();
        assert_eq!(
            w.r.backend.pending_ids().unwrap(),
            vec![rec.pending_id.clone()]
        );
        pending::discard(&w.home, &mut w.r.backend, &rec.pending_id, Utc::now()).unwrap();
        assert!(w.r.backend.pending_ids().unwrap().is_empty());
    }
}
