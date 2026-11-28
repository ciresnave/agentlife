// SPDX-License-Identifier: MIT OR Apache-2.0

//! `start` through the `Ctx` seam: a lane's spec and state in a tempdir state directory, fake
//! process facts, a fake verified caller, a recording launch spawner that never opens a window, no
//! real sleep, and the `restart.log` rows read back. The machine's state directory, ledger and
//! windows are never touched.

use super::*;
use crate::app::run_with;
use crate::app::test_fixtures::{coming_up_spawn, failing_spawn, fixture, live_facts};
use crate::start::CallerError;
use crate::testing::{log, RecordingFacts};
use std::cell::RefCell;
use std::ffi::OsString;
use std::path::Path;
use std::rc::Rc;

fn code_is(code: ExitCode, want: u8) -> bool {
    format!("{code:?}") == format!("{:?}", ExitCode::from(want))
}

fn args(rest: &[&str]) -> Vec<OsString> {
    std::iter::once("lane-restart")
        .chain(rest.iter().copied())
        .map(OsString::from)
        .collect()
}

/// No process has a parent, so there is no launcher by ancestry either.
struct NoParents;
impl crate::lane_state_writer::ParentProcess for NoParents {
    fn parent_of(&self, _: u32) -> Option<(u32, String)> {
        None
    }
    fn cmdline_of(&self, _: u32) -> Option<Vec<String>> {
        None
    }
}

/// A context over `state` whose verified caller is `caller` (`None`: it cannot be verified).
fn ctx(
    state: &Path,
    caller: Option<&'static str>,
    facts: RecordingFacts,
    calls: &Rc<RefCell<u32>>,
) -> Ctx {
    let make: Box<dyn Fn() -> Result<String, CallerError>> = match caller {
        Some(role) => Box::new(move || Ok(role.to_string())),
        None => Box::new(|| Err(CallerError::NoClaudeAncestor)),
    };
    Ctx::capture(state.to_path_buf(), Ok(state.to_path_buf()), None)
        .with_facts(facts)
        .with_parents(NoParents)
        .with_caller(make)
        .with_launch_spawn(failing_spawn(calls))
        .with_sleeper(Rc::new(|_| {}))
}

fn dead() -> RecordingFacts {
    RecordingFacts::new(&log())
}

fn rows(state: &Path) -> Vec<crate::ledger::Row> {
    crate::ledger::read(&state.join("restart.log"))
        .map(|l| l.rows)
        .unwrap_or_default()
}

fn spec_bytes(state: &Path) -> Vec<u8> {
    std::fs::read(state.join("alpha.launch.json")).unwrap()
}

#[test]
fn a_non_pm_caller_is_refused_by_code_and_recorded_and_nothing_is_spawned() {
    let d = tempfile::tempdir().unwrap();
    let (state, _) = fixture(d.path(), "alpha", "# h\n");
    let before = spec_bytes(&state);
    let calls = Rc::new(RefCell::new(0));
    let mut c = ctx(&state, Some("beta"), dead(), &calls);
    let code = run_with(
        &mut c,
        args(&["start", "--role", "alpha", "--reason", "a test start"]),
    );
    assert!(!code_is(code, 0), "{code:?}");
    let (out, err) = c.captured();
    assert_eq!(out, "");
    assert!(err.contains("lane-restart start: refused - "), "{err}");
    let logged = rows(&state);
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert_eq!(logged[0].kind.as_deref(), Some("start"));
    assert_eq!(logged[0].requested_by, "beta");
    assert_eq!(logged[0].result.as_deref(), Some("refused:not-pm"));
    assert_eq!(logged[0].spawned, Some(false));
    assert!(!logged[0].acted);
    assert_eq!(*calls.borrow(), 0);
    assert_eq!(
        spec_bytes(&state),
        before,
        "a refused start changed the spec"
    );
}

#[test]
fn an_unverifiable_caller_is_refused_and_recorded_as_unverified() {
    let d = tempfile::tempdir().unwrap();
    let (state, _) = fixture(d.path(), "alpha", "# h\n");
    let calls = Rc::new(RefCell::new(0));
    let mut c = ctx(&state, None, dead(), &calls);
    let code = run_with(
        &mut c,
        args(&["start", "--role", "alpha", "--reason", "a test start"]),
    );
    assert!(!code_is(code, 0));
    let logged = rows(&state);
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert_eq!(logged[0].requested_by, "unverified");
    assert_eq!(logged[0].result.as_deref(), Some("refused:not-pm"));
    assert_eq!(*calls.borrow(), 0);
}

#[test]
fn a_pm_dry_run_prints_the_launch_changes_nothing_and_records_a_dry_run() {
    let d = tempfile::tempdir().unwrap();
    let (state, _) = fixture(d.path(), "alpha", "# h\n");
    let before = spec_bytes(&state);
    let calls = Rc::new(RefCell::new(0));
    let mut c = ctx(&state, Some("pm"), dead(), &calls);
    let code = run_with(
        &mut c,
        args(&[
            "start",
            "--role",
            "alpha",
            "--reason",
            "a test start",
            "--dry-run",
            "--json",
        ]),
    );
    let (out, err) = c.captured();
    assert!(code_is(code, 0), "{code:?}\n{out}\n{err}");
    let mut lines = out.lines();
    let first = lines.next().unwrap();
    assert!(
        first.starts_with("lane-restart start: DRY RUN - would launch alpha via wt.exe"),
        "{first}"
    );
    let json: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert_eq!(json["role"], "alpha");
    assert_eq!(json["outcome"], "dry-run");
    assert_eq!(json["reason"], "dry-run");
    assert_eq!(lines.next(), None);
    let logged = rows(&state);
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert_eq!(logged[0].result.as_deref(), Some("dry-run:dry-run"));
    assert_eq!(logged[0].requested_by, "pm");
    assert!(!logged[0].acted);
    assert_eq!(logged[0].spawned, Some(false));
    assert_eq!(*calls.borrow(), 0);
    assert_eq!(spec_bytes(&state), before);
    assert!(
        std::fs::read_dir(&state).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("lock")),
        "a dry run took a lock"
    );
}

#[test]
fn a_lane_that_is_already_running_is_refused_as_already_running() {
    let d = tempfile::tempdir().unwrap();
    let (state, _) = fixture(d.path(), "alpha", "# h\n");
    let calls = Rc::new(RefCell::new(0));
    let mut c = ctx(&state, Some("pm"), live_facts(), &calls);
    let code = run_with(
        &mut c,
        args(&[
            "start",
            "--role",
            "alpha",
            "--reason",
            "a test start",
            "--json",
        ]),
    );
    assert!(!code_is(code, 0));
    let (out, _) = c.captured();
    let json: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(json["outcome"], "already-running");
    let logged = rows(&state);
    assert_eq!(logged.len(), 1);
    assert_eq!(
        logged[0].result.as_deref(),
        Some("already-running:already-running")
    );
    assert_eq!(*calls.borrow(), 0);
}

#[test]
fn a_spawn_that_fails_is_recorded_failed_but_spawned_and_exits_nonzero() {
    let d = tempfile::tempdir().unwrap();
    let (state, _) = fixture(d.path(), "alpha", "# h\n");
    let before = spec_bytes(&state);
    let calls = Rc::new(RefCell::new(0));
    let mut c = ctx(&state, Some("pm"), dead(), &calls);
    let code = run_with(
        &mut c,
        args(&[
            "start",
            "--role",
            "alpha",
            "--reason",
            "a test start",
            "--json",
        ]),
    );
    assert!(code_is(code, 1), "{code:?}");
    assert_eq!(*calls.borrow(), 1, "the spawner was not used exactly once");
    let (out, err) = c.captured();
    assert!(
        err.contains("lane-restart start: launching alpha on sonnet..."),
        "{err}"
    );
    assert!(err.contains("no window in a test"), "{err}");
    let json: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(json["outcome"], "failed");
    assert_eq!(json["reason"], "spawn-failed");
    let logged = rows(&state);
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert_eq!(logged[0].result.as_deref(), Some("failed:spawn-failed"));
    assert_eq!(logged[0].spawned, Some(true));
    assert!(!logged[0].acted);
    assert_eq!(logged[0].model.as_deref(), Some("sonnet"));
    assert_eq!(
        spec_bytes(&state),
        before,
        "a failed spawn rewrote the spec"
    );
}

/// A launcher caller is held to the memory floor, read from the environment and the machine through
/// the context; the same start with enough memory gets as far as the spawn.
#[test]
fn a_launcher_start_is_held_for_memory_and_proceeds_with_enough() {
    let d = tempfile::tempdir().unwrap();
    let (state, _) = fixture(d.path(), "alpha", "# h\n");
    let home = d.path().join("agentlife-home");
    std::fs::create_dir_all(&home).unwrap();
    let home_s = home.to_string_lossy().into_owned();
    let run = |free: f64, calls: &Rc<RefCell<u32>>| {
        // `restore.lock` records this process's identity, which the fake facts must know.
        let mut facts = dead();
        facts.live_pid = Some((std::process::id(), 100));
        let mut c = ctx(&state, Some("launcher:agentlife-launcher"), facts, calls)
            .with_env(&[
                ("AGENTLIFE_HOME", &home_s),
                ("AGENTLIFE_FREE_RAM_FLOOR_GB", "8"),
            ])
            .with_free_memory_gb(Some(free));
        let code = run_with(
            &mut c,
            args(&[
                "start",
                "--role",
                "alpha",
                "--reason",
                "mail waiting",
                "--signal-id",
                "alpha:m1",
                "--json",
            ]),
        );
        (code, c.captured())
    };
    let calls = Rc::new(RefCell::new(0));
    let (code, (out, _)) = run(1.0, &calls);
    assert!(!code_is(code, 0));
    let json: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(json["reason"], "held-for-memory", "{out}");
    assert_eq!(
        json["detail"], "held for memory: 1.0 GB free, floor 8 GB",
        "the floor and the measured free memory are in the answer: {out}"
    );
    assert_eq!(*calls.borrow(), 0, "a held start spawned");
    // control: with memory above the floor the same call reaches the (failing) spawn
    let (code, (out, _)) = run(100.0, &calls);
    assert!(code_is(code, 1), "{code:?}");
    let json: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(json["reason"], "spawn-failed", "{out}");
    assert!(
        json.get("detail").is_none(),
        "only a refusal has detail: {out}"
    );
    assert_eq!(*calls.borrow(), 1);
    let logged = rows(&state);
    assert_eq!(logged.len(), 2, "{logged:?}");
    assert_eq!(logged[0].requested_by, "launcher:agentlife-launcher");
    assert_eq!(logged[0].spawned, Some(false));
    assert_eq!(logged[1].spawned, Some(true));
}

#[test]
fn a_start_whose_session_comes_up_is_recorded_started_and_the_spec_marked_running() {
    let d = tempfile::tempdir().unwrap();
    let (state, _) = fixture(d.path(), "alpha", "# h\n");
    let calls = Rc::new(RefCell::new(0));
    let mut c =
        ctx(&state, Some("pm"), dead(), &calls).with_launch_spawn(coming_up_spawn(&state, &calls));
    let code = run_with(
        &mut c,
        args(&[
            "start",
            "--role",
            "alpha",
            "--reason",
            "a test start",
            "--json",
        ]),
    );
    let (out, err) = c.captured();
    assert!(code_is(code, 0), "{code:?}\n{out}\n{err}");
    assert_eq!(*calls.borrow(), 1);
    let json: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(json["outcome"], "started");
    assert_eq!(json["reason"], "started");
    assert!(json["launch_to_live_secs"].is_u64(), "{out}");
    let logged = rows(&state);
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert_eq!(logged[0].result.as_deref(), Some("started:started"));
    assert!(logged[0].acted);
    assert_eq!(logged[0].spawned, Some(true));
    let spec = crate::spec::read(&state, "alpha").unwrap();
    assert_eq!(spec.written_by, crate::spec::WrittenBy::Launch);
    assert_eq!(spec.state, crate::spec::SpecState::Running);
}

#[test]
fn a_session_stuck_at_the_dev_channels_dialog_is_reported_awaiting_with_exit_2() {
    let d = tempfile::tempdir().unwrap();
    let (state, _) = fixture(d.path(), "alpha", "# h\n");
    let calls = Rc::new(RefCell::new(0));
    // The spawn succeeds and nothing ever writes progress; a process is seen, so after the progress
    // timeout the launch is reported as waiting for the person. The sleep is virtual.
    let spawned = Rc::clone(&calls);
    let mut c = ctx(&state, Some("pm"), dead(), &calls)
        .with_launch_spawn(Rc::new(move |_, _| {
            *spawned.borrow_mut() += 1;
            Ok(())
        }))
        .with_liveness_timing(crate::launch::LivenessTiming {
            progress_timeout: std::time::Duration::from_secs(1),
            appear_timeout: std::time::Duration::from_secs(2),
            total_timeout: std::time::Duration::from_secs(3),
            poll_interval: std::time::Duration::from_secs(1),
        });
    let code = run_with(
        &mut c,
        args(&[
            "start",
            "--role",
            "alpha",
            "--reason",
            "a test start",
            "--json",
        ]),
    );
    let (out, err) = c.captured();
    assert!(code_is(code, 2), "{code:?}\n{out}\n{err}");
    let lines: Vec<serde_json::Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert_eq!(lines[0]["event"], "lane-restart-status");
    assert_eq!(lines[0]["outcome"], "awaiting_confirmation");
    assert_eq!(lines[0]["role"], "alpha");
    assert_eq!(lines[1]["reason"], "awaiting-dialog");
    let logged = rows(&state);
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert_eq!(logged[0].result.as_deref(), Some("started:awaiting-dialog"));
    assert!(logged[0].acted);
}

fn tight_disk(_: &Path) -> Result<crate::disk::GuardOk, crate::disk::Refusal> {
    Err(crate::disk::Refusal {
        mount: "/".into(),
        free: 1 << 30,
        floor: 20 << 30,
    })
}

#[test]
fn a_volume_below_the_disk_floor_refuses_the_start_and_spawns_nothing() {
    let d = tempfile::tempdir().unwrap();
    let (state, _) = fixture(
        d.path(),
        "alpha",
        "# h
",
    );
    let calls = Rc::new(RefCell::new(0));
    let mut c = ctx(&state, Some("pm"), dead(), &calls).with_disk(tight_disk);
    let code = run_with(
        &mut c,
        args(&[
            "start",
            "--role",
            "alpha",
            "--reason",
            "a test start",
            "--json",
        ]),
    );
    assert!(code_is(code, 1), "{code:?}");
    assert_eq!(*calls.borrow(), 0, "a refused start spawned");
    let (out, err) = c.captured();
    assert!(
        err.contains(
            "lane-restart start: refused - disk floor: / has 1.0 GB free, below the 20.0 GB floor"
        ),
        "{err}"
    );
    let json: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(json["reason"], "disk-floor", "{out}");
    // control: the same call on the default (roomy) disk reaches the spawn
    let mut c = ctx(&state, Some("pm"), dead(), &calls);
    let code = run_with(
        &mut c,
        args(&[
            "start",
            "--role",
            "alpha",
            "--reason",
            "a test start",
            "--json",
        ]),
    );
    assert!(code_is(code, 1), "{code:?}");
    assert_eq!(*calls.borrow(), 1);
}

#[test]
fn an_unreadable_disk_is_a_warning_not_a_refusal() {
    let d = tempfile::tempdir().unwrap();
    let (state, _) = fixture(
        d.path(),
        "alpha",
        "# h
",
    );
    let calls = Rc::new(RefCell::new(0));
    let mut c = ctx(&state, Some("pm"), dead(), &calls)
        .with_disk(|_| Ok(crate::disk::GuardOk::Unknown("no probe".into())));
    let _ = run_with(
        &mut c,
        args(&[
            "start",
            "--role",
            "alpha",
            "--reason",
            "a test start",
            "--json",
        ]),
    );
    let err = c.captured().1;
    assert!(
        err.contains("lane-restart start: warning - free disk space was not checked: no probe"),
        "{err}"
    );
    assert_eq!(
        *calls.borrow(),
        1,
        "an unreadable disk must not block the launch"
    );
}

#[test]
fn a_start_logs_the_roster_epoch_beside_the_cached_epoch_and_warns_when_ahead() {
    let d = tempfile::tempdir().unwrap();
    let (state, _) = fixture(
        d.path(),
        "alpha",
        "# h
",
    );
    // A lane home whose session file caches epoch 6, and an mcp file naming that home.
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("sessions")).unwrap();
    std::fs::write(
        home.join("sessions").join("alpha.json"),
        r#"{"instance_id":"i","token":"SECRET","global_id":"alpha@a","epoch":6}"#,
    )
    .unwrap();
    let mcp = d.path().join("alpha.mcp.json");
    let cfg = serde_json::json!({"mcpServers":{"synapse":{"env":{"SYNAPSE_HOME": home.to_string_lossy()}}}});
    std::fs::write(&mcp, cfg.to_string()).unwrap();
    let mut spec = crate::spec::read(&state, "alpha").unwrap();
    spec.launch_args.extend([
        "--mcp-config".to_string(),
        mcp.to_string_lossy().into_owned(),
    ]);
    crate::spec::write_atomic(&state, &spec).unwrap();
    let calls = Rc::new(RefCell::new(0));
    let mut c = ctx(&state, Some("pm"), dead(), &calls)
        .with_launch_spawn(coming_up_spawn(&state, &calls))
        .with_roster_epoch(Box::new(|_, role| {
            assert_eq!(role, "alpha");
            Ok(Some(7))
        }));
    let code = run_with(
        &mut c,
        args(&["start", "--role", "alpha", "--reason", "epoch test"]),
    );
    let (out, err) = c.captured();
    assert!(
        code_is(code, 0),
        "{code:?}
{out}
{err}"
    );
    assert!(err.contains("roster 7 cached 6"), "{err}");
    assert!(err.contains("WARNING"), "{err}");
    assert!(!err.contains("SECRET") && !out.contains("SECRET"));
}

/// O22(a): a launcher start whose cache is behind the roster, with the role offline, claims from the lane
/// home and says so; an online role is never claimed; a non-launcher start does not claim.
fn reclaim_case(caller: &'static str, online: bool) -> (usize, String) {
    let d = tempfile::tempdir().unwrap();
    let (state, _) = fixture(
        d.path(),
        "alpha",
        "# h
",
    );
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("sessions")).unwrap();
    std::fs::write(
        home.join("sessions").join("alpha.json"),
        r#"{"instance_id":"i","token":"SECRET","global_id":"alpha@a","epoch":6}"#,
    )
    .unwrap();
    let mcp = d.path().join("alpha.mcp.json");
    let cfg = serde_json::json!({"mcpServers":{"synapse":{"env":{"SYNAPSE_HOME": home.to_string_lossy()}}}});
    std::fs::write(&mcp, cfg.to_string()).unwrap();
    let mut spec = crate::spec::read(&state, "alpha").unwrap();
    spec.launch_args.extend([
        "--mcp-config".to_string(),
        mcp.to_string_lossy().into_owned(),
    ]);
    crate::spec::write_atomic(&state, &spec).unwrap();
    let calls = Rc::new(RefCell::new(0));
    let claims = Rc::new(RefCell::new(Vec::<(std::path::PathBuf, String)>::new()));
    let seen = Rc::clone(&claims);
    let ah = d.path().join("agentlife-home");
    std::fs::create_dir_all(&ah).unwrap();
    let ah_s = ah.to_string_lossy().into_owned();
    let mut facts = dead();
    facts.live_pid = Some((std::process::id(), 100));
    let launcher = caller.starts_with("launcher:");
    let mut c = ctx(&state, Some(caller), facts, &calls)
        .with_env(&[
            ("AGENTLIFE_HOME", &ah_s),
            ("AGENTLIFE_FREE_RAM_FLOOR_GB", "8"),
        ])
        .with_free_memory_gb(Some(100.0))
        .with_launch_spawn(coming_up_spawn(&state, &calls))
        .with_roster_epoch(Box::new(|_, _| Ok(Some(7))))
        .with_roster_online(Box::new(move |_, _| Ok(Some(online))))
        .with_role_claim(Box::new(move |h, r| {
            seen.borrow_mut().push((h.to_path_buf(), r.to_string()));
            Ok(())
        }));
    let mut a = vec!["start", "--role", "alpha", "--reason", "o22a"];
    if launcher {
        a.extend(["--signal-id", "alpha:m1"]);
    }
    let code = run_with(&mut c, args(&a));
    let (out, err) = c.captured();
    assert!(
        code_is(code, 0) || code_is(code, 2),
        "{code:?}
{out}
{err}"
    );
    assert!(!err.contains("SECRET") && !out.contains("SECRET"));
    let n = claims.borrow().len();
    if n == 1 {
        assert_eq!(claims.borrow()[0], (home, "alpha".to_string()));
    }
    (n, err)
}

#[test]
fn a_launcher_start_with_the_cache_behind_and_the_role_offline_claims_from_the_lane_home_and_logs_it(
) {
    let (n, err) = reclaim_case("launcher:agentlife-launcher", false);
    assert_eq!(n, 1, "{err}");
    assert!(err.contains("reclaimed alpha"), "{err}");
}

#[test]
fn a_launcher_start_never_claims_over_an_online_role_and_a_manual_start_does_not_claim() {
    let (n, err) = reclaim_case("launcher:agentlife-launcher", true);
    assert_eq!(n, 0, "{err}");
    assert!(err.contains("online"), "{err}");
    let (n, err) = reclaim_case("pm", false);
    assert_eq!(n, 0, "{err}");
}

/// O22(b): what a launcher start does with the lane's first prompt and its JSON when the channel is not expected
/// to connect. Returns (the spawned command line, the JSON line on stdout).
fn notice_case(
    caller: &'static str,
    online: Vec<Result<Option<bool>, String>>,
    claim_ok: bool,
) -> (String, String) {
    let d = tempfile::tempdir().unwrap();
    let (state, _) = fixture(
        d.path(),
        "alpha",
        "# h
",
    );
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join("sessions")).unwrap();
    std::fs::write(
        home.join("sessions").join("alpha.json"),
        r#"{"instance_id":"i","token":"SECRET","global_id":"alpha@a","epoch":6}"#,
    )
    .unwrap();
    let mcp = d.path().join("alpha.mcp.json");
    let cfg = serde_json::json!({"mcpServers":{"synapse":{"env":{"SYNAPSE_HOME": home.to_string_lossy()}}}});
    std::fs::write(&mcp, cfg.to_string()).unwrap();
    let mut spec = crate::spec::read(&state, "alpha").unwrap();
    spec.launch_args.extend([
        "--mcp-config".to_string(),
        mcp.to_string_lossy().into_owned(),
    ]);
    crate::spec::write_atomic(&state, &spec).unwrap();
    let ah = d.path().join("agentlife-home");
    std::fs::create_dir_all(&ah).unwrap();
    let ah_s = ah.to_string_lossy().into_owned();
    let mut facts = dead();
    facts.live_pid = Some((std::process::id(), 100));
    let calls = Rc::new(RefCell::new(0));
    let argv = Rc::new(RefCell::new(String::new()));
    let inner = coming_up_spawn(&state, &calls);
    let seen = Rc::clone(&argv);
    let spawn: Rc<crate::launch::SpawnFn> = Rc::new(move |s, a| {
        *seen.borrow_mut() = a.hosted().join(" ");
        inner(s, a)
    });
    let reads = RefCell::new(0usize);
    let mut c = ctx(&state, Some(caller), facts, &calls)
        .with_env(&[
            ("AGENTLIFE_HOME", &ah_s),
            ("AGENTLIFE_FREE_RAM_FLOOR_GB", "8"),
        ])
        .with_free_memory_gb(Some(100.0))
        .with_launch_spawn(spawn)
        .with_roster_epoch(Box::new(|_, _| Ok(Some(7))))
        .with_roster_online(Box::new(move |_, _| {
            // The n-th read answers `online[n]`; the last answer repeats.
            let n = reads.replace_with(|n| *n + 1);
            online[n.min(online.len() - 1)].clone()
        }))
        .with_role_claim(Box::new(move |_, _| {
            if claim_ok {
                Ok(())
            } else {
                Err("claim refused".to_string())
            }
        }));
    let mut a = vec!["start", "--role", "alpha", "--reason", "o22b", "--json"];
    if caller.starts_with("launcher:") {
        a.extend(["--signal-id", "alpha:m1"]);
    }
    let code = run_with(&mut c, args(&a));
    let (out, err) = c.captured();
    assert!(
        code_is(code, 0),
        "{code:?}
{out}
{err}"
    );
    let spawned = argv.borrow().clone();
    (spawned, out)
}

#[test]
fn a_launcher_start_whose_channel_will_not_connect_says_so_in_the_first_prompt_and_the_json() {
    const L: &str = "launcher:agentlife-launcher";
    // online elsewhere, a claim that fails, and an online state that cannot be read: each is told.
    for (online, claim_ok, why) in [
        (vec![Ok(Some(true))], true, "held"),
        (vec![Ok(Some(false))], false, "claim"),
        (vec![Err("synapse down".to_string())], true, "unread"),
        // went online between the first read and the claim
        (vec![Ok(Some(false)), Ok(Some(true))], true, "raced"),
        // a zero-exit claim after which the roster does not list the role
        (
            vec![Ok(Some(false)), Ok(Some(false)), Ok(None)],
            true,
            "unlisted",
        ),
    ] {
        let (argv, out) = notice_case(L, online, claim_ok);
        assert!(
            argv.contains("NOTICE: your Synapse channel may not connect"),
            "{why}: {argv}"
        );
        let json: serde_json::Value =
            serde_json::from_str(out.trim().lines().last().unwrap()).unwrap();
        assert!(
            json["notice"]
                .as_str()
                .is_some_and(|n| n.contains("may not connect")),
            "{why}: {out}"
        );
    }
    // controls: a reclaimed role, a manual start, and an unchanged cache carry no notice.
    let (argv, out) = notice_case(L, vec![Ok(Some(false))], true);
    assert!(
        !argv.contains("NOTICE") && !out.contains("notice"),
        "{argv}
{out}"
    );
    let (argv, out) = notice_case("pm", vec![Ok(Some(true))], true);
    assert!(
        !argv.contains("NOTICE") && !out.contains("notice"),
        "{argv}
{out}"
    );
}
