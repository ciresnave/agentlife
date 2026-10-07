// SPDX-License-Identifier: MIT OR Apache-2.0
//! The launcher against real processes (M3b): a plan built from the **real registry** (written by
//! real stand-in `claude` processes through the real hook) is executed, each agent is a stand-in
//! behind a stand-in `lane-restart host`, and the hook of the **new** session registers it again.
//!
//! Nothing here starts a real lane: every process is a stand-in this file built, and every one is
//! killed at the end. `agentlife restore` without `--dry-run` still refuses; these tests call the
//! library directly.
//!
//! Run from inside a Claude Code session, the stand-ins would see a real `claude` above them and the
//! hook would (correctly) skip them; run this detached (CI is not under one).

use agentlife::clock::SystemClock;
use agentlife::config::{Config, Layer};
use agentlife::down::{SysinfoTerminator, Terminator};
use agentlife::home::Home;
use agentlife::identity::{ProcessIdentity, ProcessTable, SnapshotTable, SysinfoTable};
use agentlife::journal::Journal;
use agentlife::launch::{
    build_tab, Programs, SpawnError, SpawnVia, Spawner, TabLaunch, SESSION_IDENTITY_ENV_VARS,
};
use agentlife::plan::{self, Entry, Plan};
use agentlife::registry::{AgentRecord, Registry};
use agentlife::restore::{
    execute, preflight, program_exists, write_report, Deps, Outcome, RegistryObserver, Report,
    RestoreLock,
};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const AGENTLIFE: &str = env!("CARGO_BIN_EXE_agentlife");

fn example(name: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("test exe path");
    let debug = exe.parent().and_then(Path::parent).expect("target dir");
    debug
        .join("examples")
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

/// Starts `c`, retrying while Linux answers "Text file busy" for a just-copied executable.
fn spawn_retrying(c: &mut Command) -> std::io::Result<Child> {
    for _ in 0..100 {
        match c.spawn() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(Duration::from_millis(50))
            }
            other => return other,
        }
    }
    c.spawn()
}

struct Rig {
    dir: tempfile::TempDir,
    home: PathBuf,
    claude: PathBuf,
    host: PathBuf,
    envlog: PathBuf,
}

impl Rig {
    fn new() -> Rig {
        let dir = tempfile::tempdir().expect("tempdir");
        let claude = dir
            .path()
            .join(format!("claude{}", std::env::consts::EXE_SUFFIX));
        let host = dir
            .path()
            .join(format!("host{}", std::env::consts::EXE_SUFFIX));
        for (name, to) in [("fake_claude", &claude), ("fake_host", &host)] {
            let src = example(name);
            assert!(
                src.exists(),
                "{} is not built: `cargo test` builds examples",
                src.display()
            );
            std::fs::copy(&src, to).expect("copy the stand-in");
        }
        let home = dir.path().join("home");
        let envlog = dir.path().join("envlog.txt");
        Rig {
            dir,
            home,
            claude,
            host,
            envlog,
        }
    }

    fn root(&self) -> String {
        self.dir.path().display().to_string()
    }

    fn lane_dir(&self, name: &str) -> PathBuf {
        let p = self.dir.path().join(name);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn registry(&self) -> Registry {
        Registry::new(self.home.join("registry").join("agents"))
    }

    fn write_config(&self, extra: serde_json::Value) {
        std::fs::create_dir_all(&self.home).unwrap();
        let mut c = serde_json::json!({
            "portfolio_root": self.root(),
            "host_program": self.host.display().to_string(),
            "claude_program": self.claude.display().to_string(),
        });
        for (k, v) in extra.as_object().unwrap() {
            c[k] = v.clone();
        }
        std::fs::write(self.home.join("config.json"), c.to_string()).unwrap();
    }

    fn config(&self) -> Config {
        Config::resolve(
            Layer::default(),
            Layer::default(),
            Layer::load_file(&self.home.join("config.json")).unwrap(),
        )
        .unwrap()
    }

    /// Environment every stand-in needs to find its own `agentlife` and home.
    fn env(&self) -> Vec<(String, String)> {
        vec![
            ("AGENTLIFE_HOME".into(), self.home.display().to_string()),
            ("FAKE_AGENTLIFE".into(), AGENTLIFE.into()),
            ("FAKE_AUTO_START".into(), "1".into()),
            ("FAKE_HOLD".into(), "40".into()),
            ("FAKE_ENVLOG".into(), self.envlog.display().to_string()),
        ]
    }

    /// A lane that "was running before the restart": a stand-in that registers itself through the
    /// real hook and then ends, leaving a wanted agent that is not alive.
    fn register_gone(&self, name: &str, cwd: &Path) {
        let mut c = Command::new(&self.claude);
        c.args([
            "-n",
            name,
            "--permission-mode",
            "auto",
            "--dangerously-load-development-channels",
            "server:claude-peers",
        ])
        .current_dir(cwd)
        .env("AGENTLIFE_HOME", &self.home)
        .env("FAKE_AGENTLIFE", AGENTLIFE)
        .env("FAKE_AUTO_START", "1")
        .env_remove("FAKE_HOLD")
        .env_remove("FAKE_REPORT")
        .env_remove("AGENTLIFE_AGENT_ID")
        .env_remove("LANE_ROLE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        let mut child = spawn_retrying(&mut c).expect("start the stand-in");
        child.wait().expect("wait for the stand-in");
    }

    fn records(&self) -> Vec<AgentRecord> {
        self.registry().list().unwrap().records
    }

    fn plan(&self, only_names: Option<&[String]>) -> Plan {
        let recs = self.records();
        plan::build(&plan::Inputs {
            records: &recs,
            table: &SnapshotTable::capture(),
            cfg: &self.config(),
            free_ram_gb: None,
            cwd_exists: &|c| Path::new(c).is_dir(),
            priority: &[],
            only: only_names,
        })
    }

    /// Ends every stand-in the registry says is alive, by pid **and** start time.
    fn kill_all(&self) {
        for rec in self.records() {
            for s in &rec.sessions {
                if let (None, Some(start)) = (s.ended_at, s.process_start_secs) {
                    let _ = SysinfoTerminator.kill_verified(&ProcessIdentity {
                        pid: s.pid,
                        start_secs: start,
                        exe: None,
                    });
                }
            }
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.kill_all();
    }
}

/// Runs the host directly, with the same environment treatment the real spawner gives a tab. It
/// exists only here: a real lane needs a real terminal, so this is never a product spawner.
struct DirectSpawner {
    env: Vec<(String, String)>,
    children: Mutex<Vec<Child>>,
}

impl DirectSpawner {
    fn new(env: Vec<(String, String)>) -> Self {
        DirectSpawner {
            env,
            children: Mutex::new(Vec::new()),
        }
    }
}

impl Spawner for DirectSpawner {
    fn spawn(&self, tab: &TabLaunch) -> Result<SpawnVia, SpawnError> {
        let (program, rest) = tab.hosted_argv.split_first().unwrap();
        let mut c = Command::new(program);
        c.args(rest).current_dir(&tab.cwd);
        for var in SESSION_IDENTITY_ENV_VARS {
            c.env_remove(var);
        }
        for (k, v) in self.env.iter().chain(tab.env_set.iter()) {
            c.env(k, v);
        }
        c.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = spawn_retrying(&mut c).map_err(|e| SpawnError(e.to_string()))?;
        self.children.lock().unwrap().push(child);
        Ok(SpawnVia::Direct)
    }
}

impl Drop for DirectSpawner {
    fn drop(&mut self) {
        for c in self.children.lock().unwrap().iter_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Plays `lane-restart`'s state writer for the named agents: once their new session exists, it
/// writes a lane-state record past `SessionStart`, which is what makes an agent Working.
fn play_state_writer(
    registry_dir: PathBuf,
    lane_state: PathBuf,
    names: Vec<String>,
    old: std::collections::HashSet<String>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        std::fs::create_dir_all(&lane_state).unwrap();
        let reg = Registry::new(registry_dir);
        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
            for rec in reg.list().map(|l| l.records).unwrap_or_default() {
                let Some(n) = rec.name.clone().filter(|n| names.contains(n)) else {
                    continue;
                };
                if let Some(s) = rec.sessions.iter().find(|s| !old.contains(&s.session_id)) {
                    let body = serde_json::json!({
                        "session_id": s.session_id,
                        "updated_at": chrono::Utc::now(),
                        "updated_by_event": "UserPromptSubmit",
                    });
                    let _ = std::fs::write(lane_state.join(format!("{n}.json")), body.to_string());
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    })
}

struct Ran {
    report: Report,
}

fn run_plan(
    rig: &Rig,
    plan: &Plan,
    spawner: &dyn Spawner,
    handoff: &dyn Fn(&Entry) -> bool,
) -> Ran {
    let cfg = rig.config();
    let home = Home::new(&rig.home).unwrap();
    let registry = rig.registry();
    let journal = Journal::new(home.journal_dir(), std::sync::Arc::new(SystemClock));
    let table = SysinfoTable;
    let lane_state = rig.dir.path().join("lane-state");
    let observer = RegistryObserver {
        registry: &registry,
        table: &table,
        lane_state_dir: &lane_state,
    };
    let programs = Programs {
        host: cfg.host_program.clone(),
        claude: cfg.claude_program.clone(),
    };
    let me = SysinfoTable
        .identity_of(std::process::id())
        .expect("this test process");
    let _lock = RestoreLock::acquire(&home, &me, &table).expect("the restore lock");
    let pre = preflight(&programs, &program_exists);
    assert!(pre.iter().all(|p| p.ok), "{pre:?}");
    let sleep = |d: Duration| std::thread::sleep(d);
    let report = execute(
        plan,
        pre,
        &Deps {
            spawner,
            observer: &observer,
            clock: &SystemClock,
            sleep: &sleep,
            memory_gb: &|| None,
            handoff_exists: handoff,
            journal: &journal,
            programs: &programs,
            poll: Duration::from_millis(250),
            progress_timeout: Duration::from_secs(cfg.progress_timeout_secs),
            stop_after_failed_batches: cfg.stop_after_failed_batches,
            spawn_gap: Duration::from_millis(cfg.spawn_gap_ms),
        },
    );
    write_report(&home, &report).expect("write the report");
    Ran { report }
}

fn outcome<'a>(r: &'a Report, name: &str) -> &'a Outcome {
    &r.entries
        .iter()
        .find(|e| e.name.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("no entry for {name}"))
        .outcome
}

fn launch_args_of(rig: &Rig, name: &str) -> (usize, Vec<String>) {
    let rec = rig
        .records()
        .into_iter()
        .find(|r| r.name.as_deref() == Some(name))
        .unwrap();
    (rec.sessions.len(), rec.launch_args.clone().unwrap())
}

#[test]
fn a_real_registry_is_planned_launched_through_a_host_and_each_agent_is_seen_coming_up() {
    let rig = Rig::new();
    rig.write_config(serde_json::json!({
        "batch_size": 2, "batch_delay_secs": 1,
        "liveness_timeout_secs": 40, "progress_timeout_secs": 3, "spawn_gap_ms": 0
    }));
    let names = ["alpha", "beta", "gamma"];
    for n in names {
        let dir = rig.lane_dir(n);
        rig.register_gone(n, &dir);
    }
    std::fs::write(rig.lane_dir("alpha").join("HANDOFF.md"), "state").unwrap();
    let before = rig.records();
    assert_eq!(before.len(), 3, "three lanes were registered");
    assert!(before.iter().all(|r| r.sessions.len() == 1));
    let old: std::collections::HashSet<String> = before
        .iter()
        .flat_map(|r| r.sessions.iter().map(|s| s.session_id.clone()))
        .collect();

    let plan = rig.plan(None);
    assert_eq!(plan.entries.len(), 3, "{plan:?}");
    assert_eq!(plan.batch_count(), 2, "batches of two");

    let spawner = DirectSpawner::new(rig.env());
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let state = play_state_writer(
        rig.home.join("registry").join("agents"),
        rig.dir.path().join("lane-state"),
        vec!["alpha".into()],
        old,
        stop.clone(),
    );
    let alpha_dir = rig.lane_dir("alpha");
    let run = run_plan(&rig, &plan, &spawner, &|e| {
        Path::new(&e.cwd) == alpha_dir.as_path()
    });
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    state.join().unwrap();
    let r = &run.report;

    // alpha shows progress past SessionStart; the others are alive and silent with the channel flag.
    assert_eq!(outcome(r, "alpha"), &Outcome::Working, "{r:#?}");
    assert_eq!(outcome(r, "beta"), &Outcome::AwaitingDialog, "{r:#?}");
    assert_eq!(outcome(r, "gamma"), &Outcome::AwaitingDialog, "{r:#?}");
    assert!(r
        .entries
        .iter()
        .all(|e| e.spawned_via == Some(SpawnVia::Direct)));
    assert!(r.halted.is_none() && !r.held_for_memory);

    // The SAME agents, each now with a second session: nobody was registered twice.
    let after = rig.records();
    assert_eq!(after.len(), 3);
    for n in names {
        let (sessions, args) = launch_args_of(&rig, n);
        assert_eq!(sessions, 2, "{n}: the new session joined the old record");
        // The argv the new claude really had: program, then the prompt FIRST, then the flags.
        assert!(
            args[0].ends_with(&format!("claude{}", std::env::consts::EXE_SUFFIX)),
            "{args:?}"
        );
        let prompt = if n == "alpha" {
            "read alpha HANDOFF and continue".to_string()
        } else {
            format!("there is no HANDOFF for {n}, so say so and wait for instructions")
        };
        assert_eq!(args[1], prompt, "{n}");
        assert_eq!(
            args[2..],
            [
                "--name",
                n,
                "--permission-mode",
                "auto",
                "--dangerously-load-development-channels",
                "server:claude-peers"
            ],
            "{n}"
        );
    }
    // The environment each new session started with: its own id, no session-identity variable.
    let log = std::fs::read_to_string(&rig.envlog).unwrap();
    for rec in &after {
        assert!(
            log.contains(&format!("agent_id={} identity=\n", rec.agent_id)),
            "{} missing from\n{log}",
            rec.agent_id
        );
    }

    // The report is on disk and the journal tells the story in order.
    let home = Home::new(&rig.home).unwrap();
    let files: Vec<_> = std::fs::read_dir(home.reports_dir())
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(files.len(), 1);
    let back: Report =
        serde_json::from_str(&std::fs::read_to_string(files[0].path()).unwrap()).unwrap();
    assert_eq!(&back, r);
    assert_eq!(back.plan_hash, plan.hash);
    let kinds: Vec<String> = Journal::new(home.journal_dir(), std::sync::Arc::new(SystemClock))
        .read_all()
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.kind)
        .filter(|k| k.starts_with("restore-"))
        .collect();
    assert_eq!(kinds.first().map(String::as_str), Some("restore-started"));
    assert_eq!(kinds.last().map(String::as_str), Some("restore-finished"));
    assert_eq!(kinds.iter().filter(|k| *k == "restore-launched").count(), 3);

    // Idempotence against the real process table: everything is up, so the next plan is empty.
    let again = rig.plan(None);
    assert!(again.entries.is_empty(), "{again:?}");
    assert_eq!(again.running_now, 3);
}

#[test]
fn a_second_restore_is_told_who_holds_the_lock_and_gets_in_when_the_owner_is_gone() {
    let rig = Rig::new();
    std::fs::create_dir_all(&rig.home).unwrap();
    let home = Home::new(&rig.home).unwrap();
    let me = SysinfoTable.identity_of(std::process::id()).unwrap();
    let table = SysinfoTable;
    let held = RestoreLock::acquire(&home, &me, &table).unwrap();
    // A second process asking: this one is the owner and alive, so it is refused naming it.
    let err = RestoreLock::acquire(&home, &me, &table).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("another restore is running (pid {})", me.pid)
    );
    drop(held);
    // A real child that took the lock and died: its identity is no longer alive, so we reclaim it.
    #[cfg(windows)]
    let mut c = {
        let mut c = Command::new("ping");
        c.args(["-n", "30", "127.0.0.1"]);
        c
    };
    #[cfg(not(windows))]
    let mut c = {
        let mut c = Command::new("sleep");
        c.arg("30");
        c
    };
    let mut child = c
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let ghost = loop {
        if let Some(i) = SysinfoTable.identity_of(child.id()) {
            break i;
        }
        assert!(
            Instant::now() < deadline,
            "the child never showed in the process table"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let _ = child.kill();
    let _ = child.wait();
    std::fs::write(
        home.root().join("restore.lock"),
        format!("{} {}", ghost.pid, ghost.start_secs),
    )
    .unwrap();
    let got = RestoreLock::acquire(&home, &me, &table);
    assert!(got.is_ok(), "{got:?}");
}

#[test]
fn twelve_real_stand_ins_are_paced_pm_first_and_never_more_than_a_batch_at_once() {
    let rig = Rig::new();
    rig.write_config(serde_json::json!({
        "batch_size": 5, "batch_delay_secs": 1,
        "liveness_timeout_secs": 40, "progress_timeout_secs": 2, "spawn_gap_ms": 0
    }));
    // The PM: named `pm`, launched in the portfolio root (the visible rule).
    let root = PathBuf::from(rig.root());
    rig.register_gone("pm", &root);
    for i in 0..11 {
        let n = format!("l{i:02}");
        let d = rig.lane_dir(&n);
        rig.register_gone(&n, &d);
    }
    let plan = rig.plan(None);
    assert_eq!(plan.entries.len(), 12, "{:?}", plan.excluded);
    assert!(plan.entries[0].pm, "the PM is first");
    // PM alone, then 5, 5, 1.
    assert_eq!(
        plan.entries.iter().map(|e| e.batch).collect::<Vec<_>>(),
        [0, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 3]
    );
    let old: std::collections::HashSet<String> = rig
        .records()
        .iter()
        .flat_map(|r| r.sessions.iter().map(|s| s.session_id.clone()))
        .collect();

    let spawner = DirectSpawner::new(rig.env());
    let began = Instant::now();
    let run = run_plan(&rig, &plan, &spawner, &|_| false);
    let took = began.elapsed();
    let r = &run.report;
    assert_eq!(
        r.summary.awaiting_dialog + r.summary.working + r.summary.started,
        12,
        "{r:#?}"
    );
    // Three delays of one second separate four batches.
    assert!(took >= Duration::from_secs(3), "{took:?}");

    // The PM's new session began before anybody else's: nothing started until it had come up.
    let new_start = |name: &str| {
        rig.records()
            .into_iter()
            .find(|x| x.name.as_deref() == Some(name))
            .unwrap()
            .sessions
            .iter()
            .find(|s| !old.contains(&s.session_id))
            .unwrap()
            .started_at
    };
    let pm_at = new_start("pm");
    for i in 0..11 {
        assert!(
            new_start(&format!("l{i:02}")) > pm_at,
            "l{i:02} began before the PM"
        );
    }
    // No more than batch_size launched-but-unsettled at any launch instant.
    let mut peak = 0;
    for probe in &r.entries {
        let p = probe.launched_at.unwrap();
        let live = r
            .entries
            .iter()
            .filter(|e| {
                let l = e.launched_at.unwrap();
                l <= p && p < l + chrono::Duration::seconds(e.seconds_to_settle.unwrap() as i64)
            })
            .count();
        peak = peak.max(live);
    }
    assert!(peak <= 5, "peak {peak}");
    // The PM led the report.
    assert!(r.render_text().contains("batch 0 pm"));
}

#[cfg(windows)]
#[test]
fn the_real_spawner_starts_stand_ins_through_conhost_and_they_register() {
    let rig = Rig::new();
    rig.write_config(serde_json::json!({
        "batch_size": 3, "batch_delay_secs": 1,
        "liveness_timeout_secs": 60, "progress_timeout_secs": 3
    }));
    for n in ["c1", "c2"] {
        let d = rig.lane_dir(n);
        rig.register_gone(n, &d);
    }
    let plan = rig.plan(None);
    assert_eq!(plan.entries.len(), 2);
    let spawner = agentlife::launch::RealSpawner {
        wt: "agentlife-no-such-wt".into(),
        conhost: "conhost.exe".into(),
        extra_env: rig.env(),
        ..agentlife::launch::RealSpawner::default()
    };
    let run = run_plan(&rig, &plan, &spawner, &|_| false);
    let r = &run.report;
    // `conhost.exe` is the fallback for a machine without `wt.exe`, and on some machines it will not
    // run a command at all (measured 2026-10-07 on a busy box: about half of all runs, each failing
    // five attempts in a row with `conhost.exe` exiting after ~270 ms). The launcher reports that
    // honestly as a failed agent; this test then has nothing to say about the launched processes.
    // CI sets AGENTLIFE_REQUIRE_CONHOST=1, so there it is a failure, never a skip.
    let conhost_unavailable = r.entries.iter().any(
        |e| matches!(&e.outcome, Outcome::Failed(w) if w.contains("did not start its command")),
    );
    if conhost_unavailable {
        assert!(
            std::env::var("AGENTLIFE_REQUIRE_CONHOST").as_deref() != Ok("1"),
            "conhost.exe would not run the stand-in here, and this run requires it: {r:#?}"
        );
        eprintln!(
            "SKIPPED: conhost.exe would not run a command in this environment (not required here)"
        );
        return;
    }
    assert!(
        r.entries
            .iter()
            .all(|e| e.spawned_via == Some(SpawnVia::Conhost)),
        "{r:#?}"
    );
    for n in ["c1", "c2"] {
        assert!(
            outcome(r, n).came_up(),
            "{n}: {r:#?}
hook.log:
{}
envlog:
{}
lane dirs:
{}
procs:
{}",
            std::fs::read_to_string(rig.home.join("hook.log")).unwrap_or_default(),
            std::fs::read_to_string(&rig.envlog).unwrap_or_default(),
            ["c1", "c2"]
                .iter()
                .map(|l| format!(
                    "{l}: {:?}",
                    std::fs::read_dir(rig.dir.path().join(l))
                        .map(|r| r.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
                ))
                .collect::<Vec<_>>()
                .join("; "),
            {
                let mut sys = sysinfo::System::new();
                sys.refresh_processes_specifics(
                    sysinfo::ProcessesToUpdate::All,
                    true,
                    sysinfo::ProcessRefreshKind::nothing().with_cmd(sysinfo::UpdateKind::Always),
                );
                let root = rig.root();
                sys.processes()
                    .values()
                    .filter(|p| p.cmd().iter().any(|a| a.to_string_lossy().contains(&root)))
                    .map(|p| format!("{} {:?}", p.name().to_string_lossy(), p.cmd()))
                    .collect::<Vec<_>>()
                    .join("; ")
            }
        );
        assert_eq!(
            launch_args_of(&rig, n).0,
            2,
            "{n} registered a second session"
        );
    }
    let log = std::fs::read_to_string(&rig.envlog).unwrap();
    assert!(
        log.lines().all(|l| l.ends_with("identity=")),
        "a session-identity variable reached a launched agent:\n{log}"
    );
    // Cleanup is verified, not assumed: every stand-in the registry says is alive is ended.
    rig.kill_all();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let alive = rig
            .records()
            .iter()
            .flat_map(|r| r.sessions.iter())
            .filter(|s| s.ended_at.is_none())
            .filter(|s| {
                s.process_start_secs.is_some_and(|st| {
                    agentlife::identity::is_same(
                        &SysinfoTable,
                        &ProcessIdentity {
                            pid: s.pid,
                            start_secs: st,
                            exe: None,
                        },
                    )
                })
            })
            .count();
        if alive == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{alive} stand-ins are still alive"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Opens real Windows Terminal tabs, so it runs only when asked: `AGENTLIFE_REAL_WT=1`. The tabs
/// run a stand-in host that writes a marker file into its working directory (the one thing
/// `wt.exe` is known to forward) and exits. Nothing is registered and no `claude` runs: the
/// environment does not reach a tab, which is exactly the thing this records for M6.
#[cfg(windows)]
#[test]
fn real_windows_terminal_opens_one_tab_per_agent_in_a_named_window() {
    if std::env::var("AGENTLIFE_REAL_WT").as_deref() != Ok("1") {
        eprintln!("skipped: set AGENTLIFE_REAL_WT=1 to open real Windows Terminal tabs");
        return;
    }
    let rig = Rig::new();
    let programs = Programs {
        host: rig.host.display().to_string(),
        claude: "claude".into(),
    };
    let mut tabs = Vec::new();
    for (i, n) in ["wt-one", "wt-two", "wt-three"].iter().enumerate() {
        let cwd = rig.lane_dir(n);
        let e = Entry {
            agent_id: format!("a-wt{i}"),
            name: Some((*n).to_string()),
            title: (*n).to_string(),
            cwd: cwd.display().to_string(),
            argv: vec!["--name".into(), (*n).to_string()],
            mode: None,
            pm: false,
            flags: vec![],
            dropped_args: vec![],
            batch: 0,
            window: 0,
            tab: i as u32,
        };
        tabs.push((cwd, build_tab(&e, &programs, true).unwrap()));
    }
    let spawner = agentlife::launch::RealSpawner::default();
    let gap = Config::default().spawn_gap_ms;
    for (_, t) in &tabs {
        let via = spawner.spawn(t).expect("wt.exe starts");
        assert_eq!(via, SpawnVia::WindowsTerminal);
        std::thread::sleep(Duration::from_millis(gap));
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    for (cwd, t) in &tabs {
        let marker = cwd.join("fake-host-marker.json");
        while !marker.exists() {
            assert!(Instant::now() < deadline, "no tab ran in {}", cwd.display());
            std::thread::sleep(Duration::from_millis(200));
        }
        std::thread::sleep(Duration::from_millis(200));
        let m: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&marker).unwrap()).unwrap();
        assert_eq!(m["role"], t.title.as_str());
        // The hosted command reached the tab intact: the prompt first, then the flags.
        let argv: Vec<String> = m["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(argv[0], "claude");
        assert_eq!(argv[1], format!("read {} HANDOFF and continue", t.title));
        assert_eq!(argv[2..], ["--name", t.title.as_str()]);
        eprintln!(
            "wt tab {}: AGENTLIFE_AGENT_ID reached it: {}",
            t.title,
            if m["agent_id"].is_null() { "NO" } else { "yes" }
        );
    }
}
