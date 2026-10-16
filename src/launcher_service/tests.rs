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
    let s = spec::SpecFile {
        schema: spec::SCHEMA,
        role: role.into(),
        name: role.into(),
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
        stop_kind: None,
        tool_version: "test".into(),
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
