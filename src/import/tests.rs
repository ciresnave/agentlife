use super::*;
use crate::identity::{ProcessIdentity, ProcessTable};
use crate::plan::{self, Reason};
use chrono::TimeZone;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 10, 15, 0, 0).unwrap()
}

fn hours_ago(h: i64) -> DateTime<Utc> {
    now() - Duration::hours(h)
}

fn state(role: &str, cwd: &str, mode: Option<&str>, age_h: i64) -> LaneState {
    LaneState {
        role: role.into(),
        session_id: format!("sess-{role}"),
        pid: 4242,
        pid_start_secs: Some(1000),
        cwd: cwd.into(),
        name: Some(role.into()),
        model: Some("claude-sonnet-5-5".into()),
        permission_mode: mode.map(Into::into),
        remote_control: false,
        busy: false,
        subagents_running: 0,
        no_background_shells: None,
        launch_args: Some(
            ["claude", "--name", role, "--model", "sonnet"]
                .map(String::from)
                .to_vec(),
        ),
        updated_at: hours_ago(age_h),
        updated_by_event: "Stop".into(),
    }
}

struct Run {
    out: Outcome,
}

fn run_with(
    states: Vec<(String, LaneState)>,
    existing: &[AgentRecord],
    park: &[&str],
    exists: &dyn Fn(&str) -> bool,
) -> Run {
    let cfg = Config::default();
    let park: Vec<String> = park.iter().map(|s| s.to_string()).collect();
    let out = decide(&Inputs {
        states: &states,
        unparsed: &[(
            "model-policy".to_string(),
            "missing field `role`".to_string(),
        )],
        existing,
        cfg: &cfg,
        now: now(),
        since: Duration::hours(DEFAULT_SINCE_HOURS),
        park: &park,
        cwd_exists: exists,
    });
    Run { out }
}

fn run(states: Vec<LaneState>) -> Run {
    run_with(
        states.into_iter().map(|s| (s.role.clone(), s)).collect(),
        &[],
        &[],
        &|_| true,
    )
}

fn reasons(r: &Run) -> Vec<(String, SkipReason)> {
    r.out
        .skipped
        .iter()
        .map(|s| (s.role.clone(), s.reason))
        .collect()
}

fn imported(r: &Run) -> Vec<&str> {
    r.out.imports.iter().map(|i| i.role.as_str()).collect()
}

#[test]
fn a_recent_lane_under_the_root_is_imported_with_its_recorded_fields() {
    let r = run(vec![state(
        "overmind",
        "C:/Projects/OverMind",
        Some("auto"),
        2,
    )]);
    let rec = &r.out.imports[0].record;
    assert_eq!(rec.name.as_deref(), Some("overmind"));
    assert_eq!(rec.role.as_deref(), Some("overmind"));
    assert_eq!(rec.launch_cwd, "C:/Projects/OverMind");
    assert_eq!(rec.origin, Origin::Imported);
    assert_eq!(rec.permission_mode.as_deref(), Some("auto"));
    assert_eq!(rec.model.as_deref(), Some("claude-sonnet-5-5"));
    assert_eq!(rec.intent, Intent::Wanted);
    assert_eq!(rec.launch_args.as_ref().unwrap()[0], "claude");
    let s = &rec.sessions[0];
    assert_eq!(
        (s.pid, s.process_start_secs, s.ended_at),
        (4242, Some(1000), None)
    );
    // Recent activity orders the restore, so first_seen is the file's own time.
    assert_eq!(rec.first_seen, hours_ago(2));
    // The one non-lane file is reported, not dropped.
    assert_eq!(
        reasons(&r),
        [("model-policy".to_string(), SkipReason::NotALaneState)]
    );
}

#[test]
fn the_age_window_is_inclusive_and_older_files_are_skipped_with_why() {
    let r = run(vec![
        state("edge", "C:/Projects/edge", None, 48),
        state("old", "C:/Projects/old", None, 49),
    ]);
    assert_eq!(imported(&r), ["edge"]);
    assert!(reasons(&r).contains(&("old".to_string(), SkipReason::TooOld)));
}

#[test]
fn a_directory_outside_the_root_or_missing_is_skipped() {
    let r = run_with(
        vec![
            (
                "away".into(),
                state("away", "C:/Users/x/AppData/away", None, 1),
            ),
            ("gone".into(), state("gone", "C:/Projects/gone", None, 1)),
            ("here".into(), state("here", "C:/Projects/here", None, 1)),
        ],
        &[],
        &[],
        &|p| p != "C:/Projects/gone",
    );
    assert_eq!(imported(&r), ["here"]);
    let got = reasons(&r);
    assert!(got.contains(&("away".to_string(), SkipReason::CwdOutsideRoot)));
    assert!(got.contains(&("gone".to_string(), SkipReason::CwdMissing)));
}

#[test]
fn test_fixture_names_and_files_without_launch_args_are_skipped() {
    let mut bare = state("bare", "C:/Projects/bare", None, 1);
    bare.launch_args = None;
    let r = run(vec![
        state("synapse-restarttest-lane", "C:/Projects/synapse", None, 1),
        bare,
        state("ok", "C:/Projects/ok", None, 1),
    ]);
    assert_eq!(imported(&r), ["ok"]);
    let got = reasons(&r);
    assert!(got.contains(&("synapse-restarttest-lane".to_string(), SkipReason::Denied)));
    assert!(got.contains(&("bare".to_string(), SkipReason::NoLaunchArgs)));
}

#[test]
fn one_record_per_name_and_directory_and_the_newest_file_wins() {
    let mut a = state("auth", "C:/Projects/auth", Some("auto"), 5);
    a.name = Some("auth".into());
    let mut b = state("auth-2", "C:/Projects/Auth/", Some("default"), 1);
    b.name = Some("AUTH".into());
    let r = run(vec![a, b]);
    assert_eq!(imported(&r), ["auth-2"]);
    assert_eq!(
        r.out.imports[0].record.permission_mode.as_deref(),
        Some("default")
    );
    assert!(reasons(&r).contains(&("auth".to_string(), SkipReason::Superseded)));
}

#[test]
fn the_same_name_in_two_directories_is_two_records() {
    let mut b = state("b", "C:/Projects/two", None, 1);
    b.name = Some("a".into());
    let r = run(vec![state("a", "C:/Projects/one", None, 1), b]);
    assert_eq!(r.out.imports.len(), 2);
}

#[test]
fn a_name_already_in_the_registry_is_not_imported_again() {
    let mut existing = AgentRecord::new(
        AgentId::new("a-1").unwrap(),
        "C:\\Projects\\overmind",
        hours_ago(30),
    );
    existing.name = Some("OverMind".into());
    let r = run_with(
        vec![(
            "overmind".into(),
            state("overmind", "C:/Projects/OverMind", None, 1),
        )],
        &[existing],
        &[],
        &|_| true,
    );
    assert!(r.out.imports.is_empty());
    assert!(reasons(&r).contains(&("overmind".to_string(), SkipReason::AlreadyRegistered)));
}

#[test]
fn park_closes_the_named_agents_only_and_reports_names_that_matched_nothing() {
    let r = run_with(
        vec![
            ("a".into(), state("a", "C:/Projects/a", None, 1)),
            ("b".into(), state("b", "C:/Projects/b", None, 1)),
        ],
        &[],
        &["A", "nobody"],
        &|_| true,
    );
    let by_role = |role: &str| {
        &r.out
            .imports
            .iter()
            .find(|i| i.role == role)
            .unwrap()
            .record
            .intent
    };
    assert!(matches!(
        by_role("a"),
        Intent::Closed {
            how: ClosedHow::Parked,
            ..
        }
    ));
    assert_eq!(by_role("b"), &Intent::Wanted);
    assert_eq!(r.out.unmatched_park, ["nobody"]);
}

#[test]
fn since_parses_one_whole_number_and_one_unit() {
    assert_eq!(parse_since("48h").unwrap(), Duration::hours(48));
    assert_eq!(parse_since("30m").unwrap(), Duration::minutes(30));
    assert_eq!(parse_since("2d").unwrap(), Duration::days(2));
    for bad in ["", "h", "0h", "-1h", "1.5h", "48", "48x", "1 h"] {
        assert!(parse_since(bad).is_err(), "{bad:?}");
    }
}

// -- what the restore plan then does with an import (R18) ------------------------------------- //

struct Alive(Vec<ProcessIdentity>);
impl ProcessTable for Alive {
    fn identity_of(&self, pid: u32) -> Option<ProcessIdentity> {
        self.0.iter().find(|p| p.pid == pid).cloned()
    }
}

fn plan_for(records: &[AgentRecord], table: &Alive) -> plan::Plan {
    let cfg = Config::default();
    plan::build(&plan::Inputs {
        records,
        table,
        cfg: &cfg,
        free_ram_gb: Some(32.0),
        cwd_exists: &|_| true,
        priority: &[],
        only: None,
    })
}

fn records_of(r: &Run) -> Vec<AgentRecord> {
    r.out.imports.iter().map(|i| i.record.clone()).collect()
}

#[test]
fn an_imported_auto_lane_is_restored_with_permission_mode_auto() {
    let r = run(vec![state(
        "overmind",
        "C:/Projects/OverMind",
        Some("auto"),
        1,
    )]);
    let p = plan_for(&records_of(&r), &Alive(vec![]));
    let argv = &p.entries[0].argv;
    let at = argv.iter().position(|a| a == "--permission-mode").unwrap();
    assert_eq!(argv[at + 1], "auto");
}

#[test]
fn an_imported_lane_with_no_recorded_mode_gets_no_mode_flag() {
    let r = run(vec![state("overmind", "C:/Projects/OverMind", None, 1)]);
    let p = plan_for(&records_of(&r), &Alive(vec![]));
    assert!(!p.entries[0]
        .argv
        .iter()
        .any(|a| a.starts_with("--permission-mode")));
}

#[test]
fn an_imported_bypass_for_a_non_pm_is_refused_not_downgraded() {
    let r = run(vec![state(
        "overmind",
        "C:/Projects/OverMind",
        Some("bypassPermissions"),
        1,
    )]);
    // The importer copies the recorded mode; it is the plan that refuses.
    assert_eq!(
        r.out.imports[0].record.permission_mode.as_deref(),
        Some("bypassPermissions")
    );
    let p = plan_for(&records_of(&r), &Alive(vec![]));
    assert!(p.entries.is_empty());
    assert_eq!(p.excluded[0].reason, Reason::BypassNotPm);
}

#[test]
fn an_imported_pm_keeps_bypass() {
    let r = run(vec![state(
        "pm",
        "C:/Projects",
        Some("bypassPermissions"),
        1,
    )]);
    let p = plan_for(&records_of(&r), &Alive(vec![]));
    assert!(p.entries[0].pm);
    assert_eq!(p.entries[0].mode.as_deref(), Some("bypassPermissions"));
}

#[test]
fn an_imported_lane_whose_process_is_still_running_is_not_restored() {
    let r = run(vec![state("overmind", "C:/Projects/OverMind", None, 1)]);
    let alive = Alive(vec![ProcessIdentity {
        pid: 4242,
        start_secs: 1000,
        exe: None,
    }]);
    let p = plan_for(&records_of(&r), &alive);
    assert!(p.entries.is_empty());
    assert_eq!(p.excluded[0].reason, Reason::Running);
    // And once the machine has rebooted (nothing holds the pid) it is restored.
    let p = plan_for(&records_of(&r), &Alive(vec![]));
    assert_eq!(p.entries.len(), 1);
}

#[test]
fn a_parked_import_is_left_alone_by_the_plan() {
    let r = run_with(
        vec![("a".into(), state("a", "C:/Projects/a", None, 1))],
        &[],
        &["a"],
        &|_| true,
    );
    let p = plan_for(&records_of(&r), &Alive(vec![]));
    assert!(p.entries.is_empty());
    assert_eq!(p.excluded[0].reason, Reason::NotWanted);
}

#[test]
fn reading_a_directory_returns_lane_files_and_reports_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let good = serde_json::to_string(&state("overmind", "C:/Projects/OverMind", None, 1)).unwrap();
    std::fs::write(dir.path().join("overmind.json"), good).unwrap();
    std::fs::write(
        dir.path().join("model-policy.json"),
        r#"{"default":"sonnet"}"#,
    )
    .unwrap();
    std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
    let (good, bad) = read_states(dir.path()).unwrap();
    assert_eq!(good.len(), 1);
    assert_eq!(good[0].0, "overmind");
    assert_eq!(bad.len(), 1);
    assert_eq!(bad[0].0, "model-policy");
}

#[test]
fn writing_creates_one_record_each_and_a_second_import_finds_them() {
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::new(dir.path().join("agents"));
    let r = run(vec![
        state("a", "C:/Projects/a", None, 1),
        state("b", "C:/Projects/b", None, 1),
    ]);
    assert_eq!(write(&reg, &r.out).unwrap(), 2);
    let listing = reg.list().unwrap();
    assert_eq!(listing.records.len(), 2);
    let again = run_with(
        vec![
            ("a".into(), state("a", "C:/Projects/a", None, 1)),
            ("b".into(), state("b", "C:/Projects/b", None, 1)),
        ],
        &listing.records,
        &[],
        &|_| true,
    );
    assert!(again.out.imports.is_empty());
    assert_eq!(
        again
            .out
            .skipped
            .iter()
            .filter(|s| s.reason == SkipReason::AlreadyRegistered)
            .count(),
        2
    );
}
