use super::*;
use crate::clock::{Clock, ManualClock};
use crate::consent::fake::{FakeConsent, COOLDOWN_SECS};
use crate::consent::{Asking, NoBackend, Outcome};
use crate::plan::{Entry, Params, PLAN_SCHEMA};
use chrono::TimeZone;
use std::cell::Cell;
use std::sync::Arc;

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 8, 12, 0, 0).unwrap()
}

fn agentlife() -> Requester {
    Requester {
        role: ROLE.into(),
        session_id: String::new(),
        claude_pid: 4242,
        claude_start_secs: 1_700_000_000,
        managed: false,
    }
}

fn entry(id: &str, mode: &str) -> Entry {
    Entry {
        agent_id: id.into(),
        name: Some(id.into()),
        title: id.into(),
        cwd: format!("C:/Projects/{id}"),
        argv: vec!["--name".into(), id.into()],
        mode: Some(mode.into()),
        pm: false,
        flags: vec![],
        dropped_args: vec![],
        batch: 1,
        window: 1,
        tab: 1,
    }
}

fn plan_with(ids: &[&str], mode: &str) -> Plan {
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
        entries: ids.iter().map(|i| entry(i, mode)).collect(),
        held: vec![],
        excluded: vec![],
        hash: String::new(),
    };
    p.hash = p.compute_hash();
    p
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

struct Rig {
    home: Home,
    _dir: tempfile::TempDir,
    clock: Arc<ManualClock>,
    consent: FakeConsent,
}

/// Lets two threads share one fake store; each call takes the mutex, so only the file lock under
/// test orders whole `create` calls.
struct ForwardConsent<'a>(&'a std::sync::Mutex<FakeConsent>);

impl Consent for ForwardConsent<'_> {
    fn submit(&mut self, req: &Request, grant: &Grant, bound_hash: &str) -> Result<String, String> {
        self.0.lock().unwrap().submit(req, grant, bound_hash)
    }
    fn begin_answer(&mut self, id: &str, bound_hash: &str) -> Result<Asking, AnswerError> {
        self.0.lock().unwrap().begin_answer(id, bound_hash)
    }
    fn resolve(&mut self, asking: &Asking, outcome: Outcome) -> Result<Resolved, String> {
        self.0.lock().unwrap().resolve(asking, outcome)
    }
    fn approved_at(
        &mut self,
        kind: crate::consent::Kind,
        subject: &str,
        requester: &Requester,
    ) -> Result<DateTime<Utc>, String> {
        self.0.lock().unwrap().approved_at(kind, subject, requester)
    }
    fn spend_one_use(
        &mut self,
        kind: crate::consent::Kind,
        subject: &str,
        requester: &Requester,
    ) -> Result<String, String> {
        self.0
            .lock()
            .unwrap()
            .spend_one_use(kind, subject, requester)
    }
    fn withdraw(&mut self, id: &str) -> Result<bool, String> {
        self.0.lock().unwrap().withdraw(id)
    }
    fn pending_ids(&self) -> Result<Vec<String>, String> {
        self.0.lock().unwrap().pending_ids()
    }
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path()).unwrap();
    let clock = Arc::new(ManualClock::new(t0()));
    let consent = FakeConsent::new(clock.clone());
    Rig {
        home,
        _dir: dir,
        clock,
        consent,
    }
}

fn made(r: &mut Rig, p: &Plan) -> PendingRestore {
    create(&r.home, &mut r.consent, p, "after the update", t0()).unwrap()
}

#[test]
fn a_created_request_is_a_record_on_disk_and_a_frozen_plan_and_asks_nobody() {
    let mut r = rig();
    let p = plan_with(&["a", "b"], "default");
    let rec = made(&mut r, &p);
    assert!(rec.is_open());
    assert_eq!(rec.plan_hash, p.hash);
    assert_eq!(rec.agents.len(), 2);
    assert_eq!(load(&r.home, &rec.pending_id).unwrap(), rec);
    assert_eq!(
        plan::load_frozen(&frozen_path(&r.home, &rec)).unwrap().plan,
        p
    );
    assert_eq!(r.consent.prompts_begun, 0, "creating never prompts");
    assert!(r.consent.granted.is_empty());
}

#[test]
fn asking_for_the_same_plan_again_returns_the_same_record() {
    let mut r = rig();
    let p = plan_with(&["a"], "default");
    let one = made(&mut r, &p);
    let two = made(&mut r, &p);
    assert_eq!(one, two);
    assert_eq!(list(&r.home).0.len(), 1);
    assert_eq!(
        std::fs::read_dir(r.home.plans_dir()).unwrap().count(),
        1,
        "one frozen file"
    );
}

#[test]
fn an_empty_plan_or_a_plan_with_a_wrong_hash_is_refused_before_anything_is_stored() {
    let mut r = rig();
    assert!(create(
        &r.home,
        &mut r.consent,
        &plan_with(&[], "default"),
        "x",
        t0()
    )
    .is_err());
    let mut bad = plan_with(&["a"], "default");
    bad.hash = "0".repeat(64);
    assert!(create(&r.home, &mut r.consent, &bad, "x", t0()).is_err());
    assert!(r.consent.pending_ids().unwrap().is_empty());
    assert!(list(&r.home).0.is_empty());
}

#[test]
fn approval_returns_the_frozen_plan_and_closes_the_record_and_spends_the_request_once() {
    let mut r = rig();
    let p = plan_with(&["a", "b"], "default");
    let rec = made(&mut r, &p);
    let prompt = Scripted::new(Outcome::Approved);
    let out = answer(&r.home, &mut r.consent, &prompt, &rec.pending_id, &p, t0()).unwrap();
    let Answered::Approved(frozen) = out else {
        panic!("expected approval, got {out:?}")
    };
    assert_eq!(frozen.plan, p);
    assert_eq!(
        load(&r.home, &rec.pending_id).unwrap().closed.unwrap().how,
        Closure::Approved
    );
    assert_eq!(r.consent.granted, std::slice::from_ref(&rec.pending_id));
    // a second approval of the same request cannot happen
    assert!(answer(&r.home, &mut r.consent, &prompt, &rec.pending_id, &p, t0()).is_err());
    assert_eq!(prompt.asked.get(), 1);
}

#[test]
fn cancel_closes_the_request_it_does_not_leave_it_pending() {
    let mut r = rig();
    let p = plan_with(&["a"], "default");
    let rec = made(&mut r, &p);
    let out = answer(
        &r.home,
        &mut r.consent,
        &Scripted::new(Outcome::Cancelled),
        &rec.pending_id,
        &p,
        t0(),
    )
    .unwrap();
    assert_eq!(out, Answered::Cancelled);
    assert_eq!(
        load(&r.home, &rec.pending_id).unwrap().closed.unwrap().how,
        Closure::Cancelled
    );
    assert!(
        !r.consent.is_pending(&rec.pending_id),
        "the consent store closed it too"
    );
    assert!(r.consent.granted.is_empty());
}

#[test]
fn a_timeout_or_an_unavailable_channel_leaves_it_pending_and_a_timeout_starts_a_cooldown() {
    let mut r = rig();
    let p = plan_with(&["a"], "default");
    let rec = made(&mut r, &p);
    let out = answer(
        &r.home,
        &mut r.consent,
        &Scripted::new(Outcome::Unavailable),
        &rec.pending_id,
        &p,
        t0(),
    )
    .unwrap();
    assert_eq!(out, Answered::StillPending);
    assert!(load(&r.home, &rec.pending_id).unwrap().is_open());

    // unavailable has no cooldown: the next try asks at once; a time-out then starts one
    let out = answer(
        &r.home,
        &mut r.consent,
        &Scripted::new(Outcome::TimedOut),
        &rec.pending_id,
        &p,
        t0(),
    )
    .unwrap();
    assert_eq!(out, Answered::StillPending);
    let again = answer(
        &r.home,
        &mut r.consent,
        &Scripted::new(Outcome::Approved),
        &rec.pending_id,
        &p,
        t0(),
    )
    .unwrap();
    let Answered::NotAsked(why) = again else {
        panic!("expected a cooldown, got {again:?}")
    };
    assert!(why.contains("cooling down"), "{why}");
    assert!(load(&r.home, &rec.pending_id).unwrap().is_open());

    r.clock
        .set(t0() + chrono::Duration::seconds(COOLDOWN_SECS + 1));
    let later = answer(
        &r.home,
        &mut r.consent,
        &Scripted::new(Outcome::Approved),
        &rec.pending_id,
        &p,
        r.clock.now(),
    )
    .unwrap();
    assert!(matches!(later, Answered::Approved(_)));
}

#[test]
fn a_pending_request_survives_a_restart_and_is_asked_again_never_approved_by_itself() {
    let mut r = rig();
    let p = plan_with(&["a"], "default");
    let rec = made(&mut r, &p);
    r.consent.simulate_restart();
    assert_eq!(list(&r.home).0.len(), 1, "the record is still there");
    assert!(r.consent.granted.is_empty(), "restoring approved nothing");
    let prompt = Scripted::new(Outcome::Unavailable);
    answer(&r.home, &mut r.consent, &prompt, &rec.pending_id, &p, t0()).unwrap();
    assert_eq!(prompt.asked.get(), 1, "the person is asked again");
    assert!(r.consent.granted.is_empty());
}

#[test]
fn a_changed_plan_voids_the_request_without_asking_and_names_the_agent_that_differs() {
    let mut r = rig();
    let old = plan_with(&["a", "b"], "default");
    let rec = made(&mut r, &old);
    let now_plan = plan_with(&["a", "b"], "acceptEdits");
    let prompt = Scripted::new(Outcome::Approved);
    let out = answer(
        &r.home,
        &mut r.consent,
        &prompt,
        &rec.pending_id,
        &now_plan,
        t0(),
    )
    .unwrap();
    let Answered::Voided { why, diffs } = out else {
        panic!("expected a void, got {out:?}")
    };
    assert!(why.contains("changed"), "{why}");
    assert!(
        diffs.iter().any(|d| d.starts_with("a:")) && diffs.iter().any(|d| d.starts_with("b:")),
        "{diffs:?}"
    );
    assert_eq!(prompt.asked.get(), 0, "never asked");
    assert!(r.consent.granted.is_empty());
    assert_eq!(
        load(&r.home, &rec.pending_id).unwrap().closed.unwrap().how,
        Closure::Voided
    );
    // the caller then asks about the rebuilt plan: a new request, a new id
    let fresh = made(&mut r, &now_plan);
    assert_ne!(fresh.pending_id, rec.pending_id);
}

#[test]
fn an_altered_consent_record_is_voided_and_never_shown() {
    let mut r = rig();
    let p = plan_with(&["a"], "default");
    let rec = made(&mut r, &p);
    r.consent
        .tamper_subject(&rec.pending_id, "restore 1 agents; also grant forever");
    let prompt = Scripted::new(Outcome::Approved);
    let out = answer(&r.home, &mut r.consent, &prompt, &rec.pending_id, &p, t0()).unwrap();
    assert!(
        matches!(&out, Answered::Voided { why, .. } if why.contains("altered")),
        "{out:?}"
    );
    assert_eq!(prompt.asked.get(), 0);
    assert!(r.consent.granted.is_empty());
}

#[test]
fn an_edited_frozen_plan_is_refused_closed_and_withdrawn() {
    let mut r = rig();
    let p = plan_with(&["a"], "default");
    let rec = made(&mut r, &p);
    let path = frozen_path(&r.home, &rec);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.contains("\"default\""),
        "the anchor for the edit exists"
    );
    std::fs::write(&path, text.replace("\"default\"", "\"bypassPermissions\"")).unwrap();
    let prompt = Scripted::new(Outcome::Approved);
    let out = answer(&r.home, &mut r.consent, &prompt, &rec.pending_id, &p, t0()).unwrap();
    assert!(
        matches!(&out, Answered::Voided { why, .. } if why.contains("altered")),
        "{out:?}"
    );
    assert_eq!(prompt.asked.get(), 0);
    assert!(
        !r.consent.is_pending(&rec.pending_id),
        "withdrawn from the store"
    );
}

#[test]
fn discard_withdraws_then_closes_and_a_closed_record_cannot_be_answered_or_discarded() {
    let mut r = rig();
    let p = plan_with(&["a"], "default");
    let rec = made(&mut r, &p);
    discard(&r.home, &mut r.consent, &rec.pending_id, t0()).unwrap();
    assert!(!r.consent.is_pending(&rec.pending_id));
    assert_eq!(
        load(&r.home, &rec.pending_id).unwrap().closed.unwrap().how,
        Closure::Discarded
    );
    assert!(discard(&r.home, &mut r.consent, &rec.pending_id, t0()).is_err());
    let prompt = Scripted::new(Outcome::Approved);
    assert!(answer(&r.home, &mut r.consent, &prompt, &rec.pending_id, &p, t0()).is_err());
    assert_eq!(prompt.asked.get(), 0);
}

#[test]
fn a_newer_plan_supersedes_the_older_open_one_which_is_withdrawn_and_kept() {
    let mut r = rig();
    let closed_old = made(&mut r, &plan_with(&["z"], "default"));
    discard(&r.home, &mut r.consent, &closed_old.pending_id, t0()).unwrap();
    let old = made(&mut r, &plan_with(&["a"], "default"));
    let new = made(&mut r, &plan_with(&["a", "b"], "default"));
    assert!(new.is_open());
    let old_now = load(&r.home, &old.pending_id).unwrap();
    let c = old_now.closed.expect("superseded");
    assert_eq!(c.how, Closure::Superseded);
    assert!(c.detail.contains(&new.pending_id));
    assert!(!r.consent.is_pending(&old.pending_id), "withdrawn");
    assert!(r.consent.is_pending(&new.pending_id));
    assert_eq!(
        load(&r.home, &closed_old.pending_id)
            .unwrap()
            .closed
            .unwrap()
            .how,
        Closure::Discarded,
        "an already closed record keeps its own closure"
    );
    let prompt = Scripted::new(Outcome::Approved);
    assert!(answer(
        &r.home,
        &mut r.consent,
        &prompt,
        &old.pending_id,
        &plan_with(&["a"], "default"),
        t0()
    )
    .is_err());
    assert_eq!(prompt.asked.get(), 0, "a superseded request is never shown");
}

#[test]
fn a_request_the_store_no_longer_holds_is_not_claimed_as_superseded() {
    let mut r = rig();
    let old = made(&mut r, &plan_with(&["a"], "default"));
    r.consent.withdraw(&old.pending_id).unwrap();
    made(&mut r, &plan_with(&["a", "b"], "default"));
    assert!(load(&r.home, &old.pending_id).unwrap().is_open());
}

#[test]
fn two_creators_at_once_leave_exactly_one_open_request() {
    use std::sync::{Condvar, Mutex};
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path()).unwrap();
    let clock = Arc::new(ManualClock::new(t0()));
    let consent = Mutex::new(FakeConsent::new(clock));
    // Each creator stops after saving its record until the other has saved too. Under the create
    // lock the second never gets that far while the first waits, so the wait times out and both
    // finish in turn. Without the lock both have saved before either scans, each supersedes the
    // other, and nothing is open: this is the interleaving the lock exists to prevent, forced.
    let saved = Arc::new((Mutex::new(0u32), Condvar::new()));
    std::thread::scope(|s| {
        for ids in [["a"], ["b"]] {
            let (home, consent, saved) = (&home, &consent, saved.clone());
            s.spawn(move || {
                crate::pending::test_hook::set_after_save(Box::new(move || {
                    let (n, cv) = &*saved;
                    let mut n = n.lock().unwrap();
                    *n += 1;
                    cv.notify_all();
                    let _ = cv
                        .wait_timeout_while(n, Duration::from_secs(2), |n| *n < 2)
                        .unwrap();
                }));
                let p = plan_with(&ids, "default");
                // the FakeConsent is not thread safe; the lock under test is the file lock
                let mut c = ForwardConsent(consent);
                create(home, &mut c, &p, "race", t0()).unwrap();
            });
        }
    });
    let open = list(&home).0.iter().filter(|x| x.is_open()).count();
    assert_eq!(open, 1);
}

#[test]
fn asking_the_same_plan_again_supersedes_nothing() {
    let mut r = rig();
    let p = plan_with(&["a"], "default");
    let one = made(&mut r, &p);
    // a different reason gives a distinct request id for the same plan
    let two = create(&r.home, &mut r.consent, &p, "another reason", t0()).unwrap();
    assert_ne!(
        one.pending_id, two.pending_id,
        "the control needs two requests"
    );
    assert!(load(&r.home, &one.pending_id).unwrap().is_open());
    assert!(load(&r.home, &two.pending_id).unwrap().is_open());
}

#[test]
fn the_last_approved_plan_is_the_baseline_and_none_before_any_approval() {
    let mut r = rig();
    assert!(last_approved_plan(&r.home).is_none());
    let p1 = plan_with(&["a"], "default");
    let rec = made(&mut r, &p1);
    assert!(
        last_approved_plan(&r.home).is_none(),
        "open is not approved"
    );
    let prompt = Scripted::new(Outcome::Approved);
    answer(&r.home, &mut r.consent, &prompt, &rec.pending_id, &p1, t0()).unwrap();
    assert_eq!(last_approved_plan(&r.home), Some(p1));
    std::fs::write(r.home.pending_dir().join("broken.json"), "{").unwrap();
    assert!(
        last_approved_plan(&r.home).is_none(),
        "an unreadable record may be the newest approval: no baseline"
    );
    std::fs::remove_file(r.home.pending_dir().join("broken.json")).unwrap();
    let p2 = plan_with(&["a", "b"], "default");
    made(&mut r, &p2);
    assert_eq!(
        last_approved_plan(&r.home).unwrap().entries.len(),
        1,
        "a newer open request is not the baseline"
    );
}

#[test]
fn no_backend_stores_nothing_and_approves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path()).unwrap();
    let p = plan_with(&["a"], "default");
    let err = create(&home, &mut NoBackend, &p, "x", t0()).unwrap_err();
    assert!(err.contains("not published"), "{err}");
    assert!(list(&home).0.is_empty());
    assert!(!home.plans_dir().exists(), "nothing was frozen either");
}

#[test]
fn an_id_cannot_name_a_file_outside_the_pending_directory() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path()).unwrap();
    for bad in ["../x", "a/b", "a\\b", "", ".."] {
        assert!(load(&home, bad).is_err(), "{bad:?}");
    }
}

#[test]
fn list_skips_temp_files_and_reports_an_unreadable_record() {
    let mut r = rig();
    let rec = made(&mut r, &plan_with(&["a"], "default"));
    std::fs::write(r.home.pending_dir().join(".x.json.tmp.1.1"), b"half").unwrap();
    std::fs::write(r.home.pending_dir().join("broken.json"), b"{").unwrap();
    let (records, problems) = list(&r.home);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].pending_id, rec.pending_id);
    assert_eq!(problems.len(), 1);
    assert!(problems[0].0.ends_with("broken.json"));
}

fn approved(r: &mut Rig, p: &Plan) -> PendingRestore {
    let rec = made(r, p);
    let out = answer(
        &r.home,
        &mut r.consent,
        &Scripted::new(Outcome::Approved),
        &rec.pending_id,
        p,
        t0(),
    )
    .unwrap();
    assert!(matches!(out, Answered::Approved(_)), "{out:?}");
    rec
}

#[test]
fn the_plan_grant_is_one_use() {
    assert_eq!(plan_consent_grant(), Grant::OneUse);
}

#[test]
fn an_approval_is_spent_on_first_use_and_the_second_is_refused() {
    let mut r = rig();
    let p = plan_with(&["a", "b"], "default");
    let rec = approved(&mut r, &p);
    let frozen =
        spend_approval(&r.home, &mut r.consent, &agentlife(), &rec.pending_id, t0()).unwrap();
    assert_eq!(frozen.plan, p);
    let again =
        spend_approval(&r.home, &mut r.consent, &agentlife(), &rec.pending_id, t0()).unwrap_err();
    assert!(again.contains("already spent"), "{again}");
}

#[test]
fn a_request_for_a_plan_with_an_unspent_approval_is_refused_until_it_is_spent() {
    let mut r = rig();
    let p = plan_with(&["a"], "default");
    let rec = approved(&mut r, &p);
    let e = create(&r.home, &mut r.consent, &p, "again", t0()).unwrap_err();
    assert!(e.contains("unspent"), "{e}");
    spend_approval(&r.home, &mut r.consent, &agentlife(), &rec.pending_id, t0()).unwrap();
    create(&r.home, &mut r.consent, &p, "again", t0()).unwrap();
}

#[test]
fn a_changed_plan_voids_the_request_unasked_so_there_is_nothing_to_spend() {
    let mut r = rig();
    let p = plan_with(&["a"], "default");
    let rec = made(&mut r, &p);
    let changed = plan_with(&["a", "b"], "default");
    let prompt = Scripted::new(Outcome::Approved);
    let out = answer(
        &r.home,
        &mut r.consent,
        &prompt,
        &rec.pending_id,
        &changed,
        t0(),
    )
    .unwrap();
    assert!(matches!(out, Answered::Voided { .. }), "{out:?}");
    assert_eq!(prompt.asked.get(), 0);
    let e =
        spend_approval(&r.home, &mut r.consent, &agentlife(), &rec.pending_id, t0()).unwrap_err();
    assert!(e.contains("not approved"), "{e}");
}

#[test]
fn an_approval_older_than_five_minutes_is_refused_and_spent() {
    let mut r = rig();
    let p = plan_with(&["a"], "default");
    let rec = approved(&mut r, &p);
    let late = t0() + chrono::Duration::seconds(APPROVAL_FRESH_SECS + 1);
    let e =
        spend_approval(&r.home, &mut r.consent, &agentlife(), &rec.pending_id, late).unwrap_err();
    assert!(e.contains("freshness"), "{e}");
    // spent by the refusal: it cannot be retried inside the window either
    let e =
        spend_approval(&r.home, &mut r.consent, &agentlife(), &rec.pending_id, t0()).unwrap_err();
    assert!(e.contains("already spent"), "{e}");
}

#[test]
fn an_approval_exactly_five_minutes_old_is_still_good() {
    let mut r = rig();
    let p = plan_with(&["a"], "default");
    let rec = approved(&mut r, &p);
    let edge = t0() + chrono::Duration::seconds(APPROVAL_FRESH_SECS);
    assert!(spend_approval(&r.home, &mut r.consent, &agentlife(), &rec.pending_id, edge).is_ok());
}

#[test]
fn an_unapproved_or_open_request_cannot_be_spent() {
    let mut r = rig();
    let p = plan_with(&["a"], "default");
    let rec = made(&mut r, &p);
    let e =
        spend_approval(&r.home, &mut r.consent, &agentlife(), &rec.pending_id, t0()).unwrap_err();
    assert!(e.contains("not been approved"), "{e}");
}

#[test]
fn the_request_is_bound_to_the_crates_plan_subject_not_a_summary_line() {
    let mut r = rig();
    let p = plan_with(&["a"], "default");
    let rec = made(&mut r, &p);
    let subject = crate::consent::restore_plan_subject(&p.hash).unwrap();
    assert_eq!(subject, format!("plan {}", p.hash));
    // the fake holds the request under that subject: approving and spending it by subject works
    let prompt = Scripted::new(Outcome::Approved);
    answer(&r.home, &mut r.consent, &prompt, &rec.pending_id, &p, t0()).unwrap();
    let at = r
        .consent
        .approved_at(Kind::RestorePlan, &subject, &agentlife())
        .unwrap();
    assert_eq!(at, t0());
}

#[test]
fn another_requester_cannot_spend_an_approval_and_it_stays_unspent() {
    let mut r = rig();
    let p = plan_with(&["a"], "default");
    let rec = approved(&mut r, &p);
    let intruder = Requester {
        role: "overmind".into(),
        ..agentlife()
    };
    let e = spend_approval(&r.home, &mut r.consent, &intruder, &rec.pending_id, t0()).unwrap_err();
    assert!(e.contains("not spending"), "{e}");
    // nothing was spent: the rightful requester still can
    spend_approval(&r.home, &mut r.consent, &agentlife(), &rec.pending_id, t0()).unwrap();
}

#[test]
fn an_approval_for_another_plan_is_not_found_by_this_plans_subject() {
    let mut r = rig();
    let other = plan_with(&["a", "b"], "default");
    let other_rec = approved(&mut r, &other);
    let p = plan_with(&["a"], "default");
    let subject = crate::consent::restore_plan_subject(&p.hash).unwrap();
    let e = r
        .consent
        .spend_one_use(Kind::RestorePlan, &subject, &agentlife())
        .unwrap_err();
    assert!(e.contains("no approval"), "{e}");
    // the other plan's approval is untouched
    spend_approval(
        &r.home,
        &mut r.consent,
        &agentlife(),
        &other_rec.pending_id,
        t0(),
    )
    .unwrap();
}

#[test]
fn the_plan_subject_is_exactly_plan_and_sixty_four_lowercase_hex() {
    use crate::consent::{parse_restore_plan_subject, restore_plan_subject};
    let h = "ab".repeat(32);
    let s = restore_plan_subject(&h).unwrap();
    assert_eq!(s, format!("plan {h}"));
    assert_eq!(parse_restore_plan_subject(&s).unwrap(), h);
    for bad in [
        "ab".repeat(31),
        "ab".repeat(33),
        "AB".repeat(32),
        "zz".repeat(32),
        String::new(),
    ] {
        assert!(restore_plan_subject(&bad).is_err(), "{bad}");
    }
    for bad in [
        h.clone(),
        format!("Plan {h}"),
        format!("plan  {h}"),
        format!("plan {h} "),
        "restore 1 agents".to_string(),
    ] {
        assert!(parse_restore_plan_subject(&bad).is_err(), "{bad}");
    }
}

#[test]
fn the_fake_refuses_a_restore_request_whose_subject_is_not_the_plan() {
    let mut r = rig();
    let p = plan_with(&["a"], "default");
    let mut req = Request {
        kind: Kind::RestorePlan,
        role: ROLE.into(),
        subject: "restore 1 agents".into(),
        summary: "s".into(),
        reason: "r".into(),
    };
    let e = r.consent.submit(&req, &Grant::OneUse, &p.hash).unwrap_err();
    assert!(e.contains("plan <hash>"), "{e}");
    req.subject = format!("plan {}", "0".repeat(64));
    let e = r.consent.submit(&req, &Grant::OneUse, &p.hash).unwrap_err();
    assert!(e.contains("different plan"), "{e}");
}

#[test]
fn the_no_backend_spends_nothing() {
    let mut c = NoBackend;
    let subject = crate::consent::restore_plan_subject(&"0".repeat(64)).unwrap();
    assert!(c
        .spend_one_use(Kind::RestorePlan, &subject, &agentlife())
        .is_err());
    assert!(c
        .approved_at(Kind::RestorePlan, &subject, &agentlife())
        .is_err());
}
