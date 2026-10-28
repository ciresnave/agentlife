// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use crate::identity::ProcessIdentity;
use crate::registry::{AgentId, AgentRecord, ClosedHow};
use chrono::Utc;
use lane_restart::spec;
use serde_json::json;

struct Nothing;
impl ProcessTable for Nothing {
    fn identity_of(&self, _: u32) -> Option<ProcessIdentity> {
        None
    }
}

// ---- the wire shapes, read from Synapse's own tests (`crates/synapsed/tests/launch.rs`) ----

#[test]
fn a_real_signal_entry_is_read_with_its_string_epoch() {
    let reply = json!({"roles": [{
        "role": "alpha", "signal_id": "alpha:alpha@acct", "epoch": "e-77",
        "attempt": 2, "count": 3, "oldest_enqueued_at": "2026-10-15T00:00:00Z"
    }]});
    let (s, skipped) = parse_signals(&reply);
    assert_eq!(skipped, 0);
    assert_eq!(
        s,
        vec![Signal {
            role: "alpha".into(),
            signal_id: "alpha:alpha@acct".into(),
            attempt: 2,
            epoch: "e-77".into(),
        }]
    );
}

#[test]
fn an_entry_that_cannot_be_answered_is_counted_not_guessed() {
    let reply = json!({"roles": [
        {"role": "alpha", "attempt": 1, "epoch": "e"},
        {"role": "beta", "signal_id": "beta:x", "attempt": 1, "epoch": "e"}
    ]});
    let (s, skipped) = parse_signals(&reply);
    assert_eq!((s.len(), skipped), (1, 1));
    assert_eq!(s[0].role, "beta");
    // A reply with no `roles` array at all is one unreadable item, not an empty queue.
    assert_eq!(parse_signals(&json!({})).1, 1);
}

#[test]
fn only_online_roles_are_taken_from_the_list_and_the_account_is_stripped() {
    let reply = json!({"roles": [
        {"global_id": "alpha@acct", "online": true},
        {"global_id": "beta@acct", "online": false},
        {"global_id": "gamma@acct", "online": null},
        {"online": true}
    ]});
    let on = parse_online(&reply);
    assert_eq!(on.into_iter().collect::<Vec<_>>(), ["alpha"]);
}

#[test]
fn daemon_refusals_map_to_what_the_cycle_acts_on() {
    use synapsed_client::MailError as M;
    let api = |k: &str| M::Api {
        kind: k.into(),
        message: "m".into(),
        current: None,
    };
    assert!(matches!(
        map_error(api("forbidden")),
        SourceError::Unauthorized(_)
    ));
    assert!(matches!(
        map_error(api("unauthorized")),
        SourceError::Unauthorized(_)
    ));
    assert!(matches!(
        map_error(api("superseded")),
        SourceError::Unauthorized(_)
    ));
    assert_eq!(map_error(api("stale_signal")), SourceError::Stale);
    assert!(matches!(
        map_error(api("malformed")),
        SourceError::Unavailable(_)
    ));
    assert!(matches!(
        map_error(M::Transport("x".into())),
        SourceError::Unavailable(_)
    ));
}

// ---- the directory ----

fn record(role: &str, intent: Intent) -> AgentRecord {
    let mut r = AgentRecord::new(
        AgentId::generate(),
        format!("C:/Projects/{role}"),
        Utc::now(),
    );
    r.role = Some(role.into());
    r.intent = intent;
    r
}

fn spec_in(dir: &Path, role: &str, state: spec::SpecState) {
    spec_in_kind(dir, role, state, None);
}

fn spec_in_kind(dir: &Path, role: &str, state: spec::SpecState, kind: Option<spec::StopKind>) {
    let s = spec::SpecFile {
        schema: spec::SCHEMA,
        role: lane_state::role::Role::new(role).unwrap(),
        name: lane_state::role::Role::new(role).unwrap(),
        cwd: format!("C:/Projects/{role}"),
        model: "sonnet".into(),
        permission_mode: None,
        remote_control: false,
        launch_args: vec![],
        handoff_path: None,
        handoff_mtime: None,
        state,
        written_at: Utc::now(),
        written_by: "test".into(),
        stopped_session_id: None,
        stopped_pid_start_secs: None,
        stop_reason: None,
        stop_kind: kind,
        tool_version: "test".into(),
        launch: None,
        wsl: None,
    };
    spec::write_atomic(dir, &s).unwrap();
}

#[test]
fn the_launcher_rule_reads_registry_intent_and_spec_state_together() {
    let reg_dir = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let reg = Registry::new(reg_dir.path());
    let closed = Intent::Closed {
        how: ClosedHow::Parked,
        by: "test".into(),
        at: Utc::now(),
    };
    for (role, intent) in [
        ("wanted", Intent::Wanted),
        ("lazy", Intent::Lazy),
        ("parked", closed),
        ("stopped", Intent::Wanted),
        ("lazy-stopped", Intent::Lazy),
    ] {
        reg.create(&record(role, intent)).unwrap();
    }
    spec_in(state.path(), "wanted", spec::SpecState::Running);
    spec_in(state.path(), "stopped", spec::SpecState::Suspended);
    spec_in(state.path(), "lazy-stopped", spec::SpecState::Stopping);
    let table = Nothing;
    let dir = RegistryDirectory {
        registry: &reg,
        lane_state_dir: state.path(),
        table: &table,
        wake_enabled: false,
    };
    assert_eq!(dir.lane("wanted"), Lane::Launchable);
    assert_eq!(
        dir.lane("lazy"),
        Lane::Launchable,
        "no spec at all is launchable"
    );
    assert_eq!(dir.lane("parked"), Lane::ClosedOnPurpose);
    assert_eq!(dir.lane("stopped"), Lane::ClosedOnPurpose);
    assert_eq!(
        dir.lane("lazy-stopped"),
        Lane::ClosedOnPurpose,
        "a lazy intent does not override a spec that records a stop"
    );
    assert_eq!(dir.lane("ghost"), Lane::Unknown);
    assert!(!dir.granted("wanted"), "no grant exists until M5");
    assert!(!dir.running("wanted"), "nothing in the table");
}

#[test]
fn an_unreadable_registry_launches_nothing() {
    let state = tempfile::tempdir().unwrap();
    let missing = state.path().join("no-such-dir").join("agents");
    let reg = Registry::new(missing);
    let table = Nothing;
    let dir = RegistryDirectory {
        registry: &reg,
        lane_state_dir: state.path(),
        table: &table,
        wake_enabled: false,
    };
    assert_ne!(dir.lane("wanted"), Lane::Launchable);
}

#[test]
fn the_notifier_only_mechanism_never_reports_a_start() {
    let m = NoMechanism.start("alpha", launcher::MODEL, "alpha:x");
    assert!(matches!(m, MechResult::Failed(_)));
    assert_eq!(
        launcher::map_mechanism(&m),
        (Outcome::Failed, "spawn-failed")
    );
}

// ---- identity ----

#[test]
fn a_second_instance_cannot_take_the_lock_and_the_first_can_after_release() {
    let d = tempfile::tempdir().unwrap();
    let first = take_instance_lock(d.path()).unwrap();
    assert!(first.is_some());
    assert!(take_instance_lock(d.path()).unwrap().is_none());
    drop(first);
    assert!(take_instance_lock(d.path()).unwrap().is_some());
}

#[test]
fn the_caller_record_is_removed_only_while_it_still_names_this_process() {
    let d = tempfile::tempdir().unwrap();
    let mine = LauncherRecord {
        role: LAUNCHER_ROLE.into(),
        pid: 4242,
        pid_start_secs: 1000,
    };
    write_caller_record(d.path(), &mine).unwrap();
    let path = d.path().join("agentlife-launcher.launcher-caller");
    assert!(path.exists());

    // A successor wrote its own record: ours must not delete it.
    let successor = LauncherRecord {
        pid: 9999,
        pid_start_secs: 2000,
        ..LauncherRecord {
            role: LAUNCHER_ROLE.into(),
            pid: 0,
            pid_start_secs: 0,
        }
    };
    write_caller_record(d.path(), &successor).unwrap();
    assert!(!remove_caller_record(d.path(), &mine));
    assert!(path.exists(), "the successor's record survives");

    assert!(remove_caller_record(d.path(), &successor));
    assert!(!path.exists());
    // Removing what is not there is not an error and removes nothing.
    assert!(!remove_caller_record(d.path(), &successor));
}

#[test]
fn the_record_lane_restart_reads_is_the_one_written_here() {
    // The file name and fields are lane-restart's own types, so a rename there breaks this build;
    // this pins the on-disk name that `launcher_caller` looks for.
    assert_eq!(
        launcher_record_name(LAUNCHER_ROLE),
        "agentlife-launcher.launcher-caller"
    );
    let rec = own_record().expect("this process has a start time");
    assert_eq!(rec.pid, std::process::id());
    assert_eq!(rec.role, LAUNCHER_ROLE);
}

// ---- review of #98 ----

#[test]
fn an_entry_without_an_epoch_is_skipped_not_answered_with_a_blank_one() {
    let reply = json!({"roles": [
        {"role": "alpha", "signal_id": "alpha:x", "attempt": 1}
    ]});
    let (s, skipped) = parse_signals(&reply);
    assert_eq!((s.len(), skipped), (0, 1));
}

fn dir_over<'a>(reg: &'a Registry, state: &'a Path, table: &'a Nothing) -> RegistryDirectory<'a> {
    RegistryDirectory {
        registry: reg,
        lane_state_dir: state,
        table,
        wake_enabled: false,
    }
}

#[test]
fn a_spec_that_exists_but_cannot_be_used_fails_closed_and_an_absent_one_does_not() {
    let reg_dir = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let reg = Registry::new(reg_dir.path());
    for role in ["absent", "corrupt", "other"] {
        reg.create(&record(role, Intent::Wanted)).unwrap();
    }
    std::fs::write(state.path().join("corrupt.launch.json"), "{ not json").unwrap();
    // a spec that is well formed but belongs to another role
    spec_in(state.path(), "someone-else", spec::SpecState::Running);
    std::fs::rename(
        state.path().join("someone-else.launch.json"),
        state.path().join("other.launch.json"),
    )
    .unwrap();
    let table = Nothing;
    let dir = dir_over(&reg, state.path(), &table);
    assert_eq!(
        dir.lane("absent"),
        Lane::Launchable,
        "no spec: the registry decides"
    );
    assert_eq!(dir.lane("corrupt"), Lane::ClosedOnPurpose);
    assert_eq!(dir.lane("other"), Lane::ClosedOnPurpose);
}

#[test]
fn the_current_record_decides_intent_not_an_older_parked_one() {
    let reg_dir = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let reg = Registry::new(reg_dir.path());
    let closed = || Intent::Closed {
        how: ClosedHow::Parked,
        by: "test".into(),
        at: Utc::now(),
    };
    let long_ago = Utc::now() - chrono::Duration::days(3);
    // an old parked agent, then a newer wanted one: the role is wanted
    let mut old = record("revived", closed());
    old.first_seen = long_ago;
    reg.create(&old).unwrap();
    reg.create(&record("revived", Intent::Wanted)).unwrap();
    // an old wanted agent, then a newer parked one: the role is closed
    let mut old = record("retired", Intent::Wanted);
    old.first_seen = long_ago;
    reg.create(&old).unwrap();
    reg.create(&record("retired", closed())).unwrap();
    let table = Nothing;
    let dir = dir_over(&reg, state.path(), &table);
    assert_eq!(dir.lane("revived"), Lane::Launchable);
    assert_eq!(dir.lane("retired"), Lane::ClosedOnPurpose);
}

// Synapse's daemon accepts exactly these four outcome strings on /v1/launch-result (its
// `Outcome::parse`; PM/Synapse lane 2026-10-16). Anything else is a 400, so each of ours must be one.
#[test]
fn every_outcome_we_post_is_one_of_the_four_the_daemon_accepts() {
    const ACCEPTED: [&str; 4] = ["started", "already-running", "refused", "failed"];
    let all = [
        Outcome::Started,
        Outcome::AlreadyRunning,
        Outcome::Refused,
        Outcome::Failed,
    ];
    let wire: Vec<&str> = all.into_iter().map(outcome_wire).collect();
    for w in &wire {
        assert!(ACCEPTED.contains(w), "{w} is not accepted by Synapse");
    }
    // each outcome posts its own string, so a swap of two accepted strings is caught
    assert_eq!(wire, ["started", "already-running", "refused", "failed"]);
    // and all four are reachable, so none is silently merged into another
    let mut sorted = wire.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 4);
}

// ---- the notify path (M8) ----

#[test]
fn the_send_body_is_what_synapsectl_send_builds() {
    let b = send_body("pm", "m-1", "hi ✓");
    assert_eq!(b["to"], "pm");
    assert_eq!(b["message_id"], "m-1");
    // Standard base64 of the UTF-8 bytes, not the text.
    assert_eq!(b["body_b64"], "aGkg4pyT");
    assert_eq!(b.as_object().unwrap().len(), 3);
}

#[test]
fn the_notify_transport_is_the_broker_unless_the_config_says_synapse() {
    use lane_restart::notify::Transport;
    assert_eq!(notify_transport(None), Transport::Broker);
    let home = tempfile::tempdir().unwrap();
    assert_eq!(
        notify_transport(Some(home.path())),
        Transport::Broker,
        "no file"
    );
    let path = lane_restart::approvals::config_path(home.path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let write = |extra: &str| {
        std::fs::write(
            &path,
            format!(r#"{{"approvals": {{"repo": "a/b", "path": "p"}}{extra}}}"#),
        )
        .unwrap()
    };
    write("");
    assert_eq!(notify_transport(Some(home.path())), Transport::Broker);
    write(r#", "notify_transport": "synapse""#);
    assert_eq!(notify_transport(Some(home.path())), Transport::Synapse);
    write(r#", "notify_transport": "carrier-pigeon""#);
    assert_eq!(
        notify_transport(Some(home.path())),
        Transport::Broker,
        "invalid falls back"
    );
}

#[test]
fn a_bare_role_is_addressed_as_role_at_account() {
    assert_eq!(qualify("pm", "scratch"), "pm@scratch");
    assert_eq!(qualify("pm@other", "scratch"), "pm@other");
}

// ---- the implied grant (board 216, docs/M5-WAKE-DESIGN.md slice 1) ----

use spec::{SpecState, StopKind};

/// Seven lanes, one per way a lane can have ended, all with a `wanted` registry record except
/// `exited-soft`, whose registry intent is what `agentlife stop --soft` leaves: `Closed{exited}`.
fn grant_fixture(reg: &Registry, state: &Path) {
    let exited = Intent::Closed {
        how: ClosedHow::Exited,
        by: "test".into(),
        at: Utc::now(),
    };
    for (role, intent, st, kind) in [
        (
            "soft",
            Intent::Wanted,
            SpecState::Suspended,
            Some(StopKind::Soft),
        ),
        (
            "soft-stopping",
            Intent::Wanted,
            SpecState::Stopping,
            Some(StopKind::Soft),
        ),
        (
            "hard",
            Intent::Wanted,
            SpecState::Suspended,
            Some(StopKind::Hard),
        ),
        ("unclassified", Intent::Wanted, SpecState::Suspended, None),
        ("crashed", Intent::Wanted, SpecState::Crashed, None),
        (
            "exited-soft",
            exited.clone(),
            SpecState::Suspended,
            Some(StopKind::Soft),
        ),
        // a parked lane whose suspend write failed leaves an earlier `running` spec behind
        ("parked-running", exited, SpecState::Running, None),
    ] {
        reg.create(&record(role, intent)).unwrap();
        spec_in_kind(state, role, st, kind);
    }
    reg.create(&record("no-spec", Intent::Wanted)).unwrap();
    reg.create(&record("corrupt", Intent::Wanted)).unwrap();
    std::fs::write(state.join("corrupt.launch.json"), "{ not json").unwrap();
}

#[test]
fn with_the_wake_path_off_nothing_is_granted_and_a_soft_stop_stays_closed() {
    let (reg_dir, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let reg = Registry::new(reg_dir.path());
    grant_fixture(&reg, state.path());
    let table = Nothing;
    let dir = dir_over(&reg, state.path(), &table);
    for role in [
        "soft",
        "soft-stopping",
        "crashed",
        "exited-soft",
        "hard",
        "no-spec",
    ] {
        assert!(
            !dir.granted(role),
            "{role}: nothing is granted while lazy is off"
        );
    }
    for role in [
        "soft",
        "soft-stopping",
        "hard",
        "unclassified",
        "exited-soft",
    ] {
        assert_eq!(
            dir.lane(role),
            Lane::ClosedOnPurpose,
            "{role}: as before 216"
        );
    }
    assert_eq!(dir.lane("crashed"), Lane::Launchable);
}

#[test]
fn with_the_wake_path_on_a_soft_stop_and_a_crash_are_granted_and_nothing_else_is() {
    let (reg_dir, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let reg = Registry::new(reg_dir.path());
    grant_fixture(&reg, state.path());
    let table = Nothing;
    let mut dir = dir_over(&reg, state.path(), &table);
    dir.wake_enabled = true;
    // (role, granted, lane)
    for (role, granted, lane) in [
        ("soft", true, Lane::Launchable),
        ("soft-stopping", true, Lane::Launchable),
        ("crashed", true, Lane::Launchable),
        // `agentlife stop --soft`: registry says exited, the spec (the authority) says soft
        ("exited-soft", true, Lane::Launchable),
        ("hard", false, Lane::ClosedOnPurpose),
        ("unclassified", false, Lane::ClosedOnPurpose),
        // a closed registry intent is not overridden by a spec that merely says `running`
        ("parked-running", false, Lane::ClosedOnPurpose),
        ("corrupt", false, Lane::ClosedOnPurpose),
        ("no-spec", false, Lane::Launchable),
        ("ghost", false, Lane::Unknown),
    ] {
        assert_eq!(dir.granted(role), granted, "granted({role})");
        assert_eq!(dir.lane(role), lane, "lane({role})");
    }
}

#[test]
fn rule_seven_and_lane_restart_start_read_one_predicate() {
    // The directory must agree with `launcher_may_start` for every (state, kind) pair.
    let (reg_dir, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let reg = Registry::new(reg_dir.path());
    let table = Nothing;
    let mut dir = dir_over(&reg, state.path(), &table);
    dir.wake_enabled = true;
    let mut n = 0;
    for st in [
        SpecState::Running,
        SpecState::Crashed,
        SpecState::Suspended,
        SpecState::Stopping,
        SpecState::FailedRelaunch,
    ] {
        for kind in [None, Some(StopKind::Soft), Some(StopKind::Hard)] {
            let role = format!("r{n}");
            n += 1;
            reg.create(&record(&role, Intent::Wanted)).unwrap();
            spec_in_kind(state.path(), &role, st, kind);
            let s = spec::read(state.path(), &role).unwrap();
            assert_eq!(
                dir.granted(&role),
                lane_restart::start::launcher_may_start(&s).is_ok(),
                "{st:?} {kind:?}"
            );
        }
    }
    assert_eq!(n, 15, "control: every pair was visited");
}
