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
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path()).unwrap();
    let clock = Arc::new(ManualClock::new(t0()));
    let consent = std::sync::Mutex::new(FakeConsent::new(clock));
    std::thread::scope(|s| {
        for ids in [["a"], ["b"]] {
            let (home, consent) = (&home, &consent);
            s.spawn(move || {
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
