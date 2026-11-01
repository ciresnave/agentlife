// SPDX-License-Identifier: MIT OR Apache-2.0
//! `agentlife`: agent lifecycle control. See `docs/MILESTONES.md`.

use agentlife::caller::{self, Caller};
use agentlife::cli::{self, Command, PendingAction};
use agentlife::clock::{Clock, SystemClock};
use agentlife::config::{Config, Layer};
use agentlife::consent;
use agentlife::control::{self, ControlError};
use agentlife::down;
use agentlife::home::Home;
use agentlife::hook::{self, Ctx, RealEnv, SnapshotParents};
use agentlife::identity::{ProcessTable, SnapshotTable, SysinfoTable};
use agentlife::intent::{self, EventLogMarker, ShutdownMarker};
use agentlife::journal::Journal;
use agentlife::list;
use agentlife::marks;
use agentlife::peers::Broker;
use agentlife::pending;
use agentlife::plan;
use agentlife::procindex::ProcIndex;
use agentlife::registry::{AgentRecord, ClosedHow, Registry};
use agentlife::run;
use agentlife::select;
use agentlife::spec_launch;
use agentlife::task;
use std::io::{Read, Write};
use std::process::ExitCode;
use std::sync::Arc;

fn main() -> ExitCode {
    match cli::parse(std::env::args().skip(1)) {
        Ok(Command::Help) => {
            print!("{}", cli::USAGE);
            ExitCode::SUCCESS
        }
        Ok(Command::Version) => {
            println!("agentlife {}", agentlife::version());
            ExitCode::SUCCESS
        }
        Ok(Command::Hook { event, reason }) => run_hook(&event, reason.as_deref()),
        Ok(Command::List { json, all }) => run_list(json, all),
        Ok(Command::Status { json }) => run_status_peers(json),
        Ok(Command::Reconcile { dry_run, json }) => run_reconcile(dry_run, json),
        Ok(Command::Pin { agent }) => run_mark(&agent, Mark::Pin),
        Ok(Command::Unpin { agent }) => run_mark(&agent, Mark::Unpin),
        Ok(Command::Waiting { agent, note, clear }) => run_waiting(agent.as_deref(), note, clear),
        Ok(Command::Park {
            agent,
            confirm,
            yes,
            timeout,
        }) => run_close(
            &agent,
            ClosedHow::Parked,
            confirm.as_deref(),
            yes,
            timeout,
            false,
        ),
        Ok(Command::Stop {
            agent,
            confirm,
            yes,
            timeout,
            soft,
        }) => run_close(
            &agent,
            ClosedHow::Exited,
            confirm.as_deref(),
            yes,
            timeout,
            soft,
        ),
        Ok(Command::Unpark { agent, .. }) => run_unpark(&agent),
        Ok(Command::Restore {
            dry_run,
            json,
            from_logon,
            only,
            priority,
            flags,
        }) => run_restore(dry_run, json, from_logon, only, priority, flags),
        Ok(Command::Pending(action)) => run_pending(action),
        Ok(Command::ImportLaneState {
            write,
            json,
            since,
            park,
        }) => run_import(write, json, since, park),
        Ok(Command::InstallTask {
            register,
            remove,
            exe,
        }) => run_install_task(register, remove, exe),
        Ok(Command::Doctor { exe }) => run_doctor(exe),
        Ok(Command::Launcher { init_store, once }) => run_launcher(init_store, once),
        Ok(Command::Worktrees { json, root, repos }) => run_worktrees(json, root, repos),
        Err(e) => {
            eprintln!("agentlife: {e}\n\n{}", cli::USAGE);
            ExitCode::FAILURE
        }
    }
}

/// A hook never fails the session: every path ends in a normal exit, and what happened is
/// written to `hook.log`.
fn run_hook(event: &str, reason: Option<&str>) -> ExitCode {
    let home = match Home::from_env() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("agentlife hook: {e}");
            return ExitCode::SUCCESS;
        }
    };
    let began = std::time::Instant::now();
    let mut stdin_json = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin_json);
    let read_ms = began.elapsed().as_millis();
    let registry = Registry::new(home.agents_dir());
    let procs = ProcIndex::new(home.procs_dir());
    let journal = Journal::new(home.journal_dir(), Arc::new(SystemClock));
    let env = RealEnv::new();
    let snapshot_ms = began.elapsed().as_millis();
    // `SessionEnd` has a 1.5 s budget and never consults the table, so it does not pay for a
    // whole-system snapshot.
    let snapshot;
    let per_pid = SysinfoTable;
    let table: &dyn ProcessTable = if event == "SessionStart" {
        snapshot = SnapshotTable::capture();
        &snapshot
    } else {
        &per_pid
    };
    let outcome = hook::run(
        &Ctx {
            registry: &registry,
            procs: &procs,
            journal: &journal,
            env: &env,
            table,
        },
        event,
        reason,
        &stdin_json,
    );
    // Timing goes in the log so a slow hook is visible: `SessionEnd` has a 1.5 s budget, and a
    // hook that blows it fails silently.
    let line = format!(
        "{} [stdin {read_ms} ms, +snapshot to {snapshot_ms} ms, total {} ms]",
        outcome.log_line_for(
            event,
            chrono::Utc::now(),
            std::env::var("AGENTLIFE_AGENT_ID").ok().as_deref()
        ),
        began.elapsed().as_millis()
    );
    let _ = std::fs::create_dir_all(home.root());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.hook_log())
    {
        let _ = writeln!(f, "{line}");
    }
    ExitCode::SUCCESS
}

/// What every non-hook command needs.
struct App {
    registry: Registry,
    journal: Journal,
    procs: ProcIndex,
    cfg: Config,
}

fn app(command: &str) -> Result<App, ExitCode> {
    let fail = |e: String| {
        eprintln!("agentlife {command}: {e}");
        ExitCode::FAILURE
    };
    let home = Home::from_env().map_err(|e| fail(e.to_string()))?;
    let cfg = Config::load(&home, Layer::default()).map_err(|e| fail(e.to_string()))?;
    Ok(App {
        registry: Registry::new(home.agents_dir()),
        journal: Journal::new(home.journal_dir(), Arc::new(SystemClock)),
        procs: ProcIndex::new(home.procs_dir()),
        cfg,
    })
}

/// `agentlife restore`. Only `--dry-run` exists in M3a: it builds the plan and prints it, and
/// writes nothing. Without it the command refuses (consent and the launcher are later milestones).
fn run_restore(
    dry_run: bool,
    json: bool,
    from_logon: bool,
    only: Option<Vec<String>>,
    priority: Vec<String>,
    flags: Vec<(String, String)>,
) -> ExitCode {
    if from_logon {
        if let Err(e) = wait_for_logon_preconditions() {
            eprintln!("agentlife restore: {e}");
            return ExitCode::FAILURE;
        }
    }
    let fail = |e: String| {
        eprintln!("agentlife restore: {e}");
        ExitCode::FAILURE
    };
    let home = match Home::from_env() {
        Ok(h) => h,
        Err(e) => return fail(e.to_string()),
    };
    let mut layer = Layer::default();
    for (k, v) in &flags {
        if let Err(e) = layer.set(k, v) {
            return fail(e.to_string());
        }
    }
    let cfg = match Config::load(&home, layer) {
        Ok(c) => c,
        Err(e) => return fail(e.to_string()),
    };
    let (p, problems) = match build_plan(&home, &cfg, &priority, only.as_deref()) {
        Ok(b) => b,
        Err(e) => return fail(e),
    };
    for (path, why) in &problems {
        eprintln!("agentlife restore: could not use {}: {why}", path.display());
    }
    if dry_run {
        if json {
            match serde_json::to_string_pretty(&p) {
                Ok(s) => println!("{s}"),
                Err(e) => return fail(e.to_string()),
            }
        } else {
            print!("{}", plan::render_text(&p, true));
        }
        return ExitCode::SUCCESS;
    }
    let reason = if from_logon {
        "restore at logon"
    } else {
        "restore requested by a person"
    };
    run_with_runtime(&home, &cfg, &priority, only.as_deref(), "restore", |rt| {
        run::restore_now(rt, &p, reason)
    })
}

/// The plan from the registry and the process table **now**, with the registry files that could not
/// be read.
fn build_plan(
    home: &Home,
    cfg: &Config,
    priority: &[String],
    only: Option<&[String]>,
) -> Result<(plan::Plan, Vec<(std::path::PathBuf, String)>), String> {
    let registry = Registry::new(home.agents_dir());
    let listing = registry.list().map_err(|e| e.to_string())?;
    let table = SnapshotTable::capture();
    let free_ram_gb = free_memory_gb();
    let p = plan::build(&plan::Inputs {
        records: &listing.records,
        table: &table,
        cfg,
        free_ram_gb,
        cwd_exists: &|c| std::path::Path::new(c).is_dir(),
        priority,
        only,
        specs: &|role| spec_launch::load(std::path::Path::new(&cfg.lane_state_dir), role),
    });
    Ok((p, listing.problems))
}

/// The directory whose volume the disk floor is measured on: the lane state directory.
fn state_dir_of(cfg: &Config) -> std::path::PathBuf {
    std::path::PathBuf::from(&cfg.lane_state_dir)
}

fn free_memory_gb() -> Option<f64> {
    let Ok(mut sys) = sysinfo::System::new() else {
        return None;
    };
    sys.refresh_memory();
    let bytes = sys.available_memory();
    (bytes > 0).then(|| bytes as f64 / (1u64 << 30) as f64)
}

/// Builds the real runtime (the shared consent store, the Hello prompt, the real launcher) and runs
/// `f` with it. **This is the only place that puts `HelloPrompt` in front of a person**, and the
/// only caller of `restore::execute` with a real spawner.
fn run_with_runtime(
    home: &Home,
    cfg: &Config,
    priority: &[String],
    only: Option<&[String]>,
    command: &str,
    f: impl FnOnce(&mut run::Runtime) -> Result<run::Ran, String>,
) -> ExitCode {
    use agentlife::consent::real::{self, HelloPrompt};
    use agentlife::launch::{platform_spawner, Programs};
    use agentlife::restore::{self, Deps, RegistryObserver};
    let fail = |e: String| {
        eprintln!("agentlife {command}: {e}");
        ExitCode::FAILURE
    };
    // The disk floor, before anyone is asked: an approval must not be spent on a run that is then
    // refused. `restore::preflight` repeats it at execution time.
    match lane_restart::disk::guard(&state_dir_of(cfg)) {
        Ok(lane_restart::disk::GuardOk::Free(_)) => {}
        Ok(lane_restart::disk::GuardOk::Unknown(why)) => {
            eprintln!("agentlife {command}: warning - free disk space was not checked: {why}")
        }
        Err(refusal) => return fail(format!("refused - {refusal}")),
    }
    let mut backend = match consent::installed() {
        Ok(b) => b,
        Err(e) => return fail(e),
    };
    let requester = match real::this_process() {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    let prompt = HelloPrompt::new();
    let procs = ProcIndex::new(home.procs_dir());
    let caller = caller::detect(&RealEnv::new(), &procs);
    let table = SysinfoTable;
    let Some(me) = table.identity_of(std::process::id()) else {
        return fail("cannot read this process from the process table".into());
    };
    let registry = Registry::new(home.agents_dir());
    let journal = Journal::new(home.journal_dir(), Arc::new(SystemClock));
    let lane_state_dir = std::path::PathBuf::from(&cfg.lane_state_dir);
    let programs = Programs {
        host: cfg.host_program.to_string(),
        claude: cfg.claude_program.to_string(),
    };
    let spawner = platform_spawner();
    let show = |text: &str| print!("{text}");
    let rebuild = || build_plan(home, cfg, priority, only).map(|(p, _)| p);
    let execute = |p: &plan::Plan| {
        // The live table, not a snapshot: the observer must see the sessions it is waiting for.
        let live = SysinfoTable;
        let observer = RegistryObserver {
            registry: &registry,
            table: &live,
            lane_state_dir: &lane_state_dir,
            hook_log: Some(home.hook_log()),
        };
        let sleep = |d: std::time::Duration| std::thread::sleep(d);
        let mut pre = restore::preflight(&programs, &restore::program_exists);
        pre.push(restore::disk_preflight(lane_restart::disk::guard(
            &state_dir_of(cfg),
        )));
        restore::execute(
            p,
            pre,
            &Deps {
                spawner: spawner.as_ref(),
                observer: &observer,
                clock: &SystemClock,
                sleep: &sleep,
                memory_gb: &free_memory_gb,
                handoff_exists: &run::entry_has_handoff,
                journal: &journal,
                programs: &programs,
                poll: std::time::Duration::from_secs(2),
                progress_timeout: std::time::Duration::from_secs(cfg.progress_timeout_secs),
                stop_after_failed_batches: cfg.stop_after_failed_batches,
                spawn_gap: std::time::Duration::from_millis(cfg.spawn_gap_ms),
            },
        )
    };
    let mut rt = run::Runtime {
        home,
        consent: backend.as_mut(),
        prompt: &prompt,
        requester: &requester,
        caller: &caller,
        clock: &SystemClock,
        table: &table,
        me: &me,
        show: &show,
        rebuild: &rebuild,
        execute: &execute,
    };
    match f(&mut rt) {
        Ok(ran) => report_ran(command, &ran),
        Err(e) => fail(e),
    }
}

/// Says truthfully what happened, and exits 0 only when every planned agent came up or was already
/// running.
fn report_ran(command: &str, ran: &run::Ran) -> ExitCode {
    use agentlife::restore::Outcome;
    match ran {
        run::Ran::Executed {
            pending_id,
            report,
            report_path,
        } => {
            println!(
                "agentlife {command}: {pending_id} approved; the approval was spent before the run"
            );
            print!("{}", report.render_text());
            match report_path {
                Ok(p) => println!("report: {}", p.display()),
                Err(e) => eprintln!(
                    "agentlife {command}: the restore ran but its report could not be written: {e}"
                ),
            }
            let all_ok = report
                .entries
                .iter()
                .all(|e| e.outcome.came_up() || matches!(e.outcome, Outcome::Skipped(_)));
            if all_ok && report.halted.is_none() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        run::Ran::Cancelled { pending_id } => {
            eprintln!("agentlife {command}: {pending_id} was cancelled; nothing was started");
            ExitCode::FAILURE
        }
        run::Ran::StillPending { pending_id } => {
            eprintln!(
                "agentlife {command}: {pending_id} was not answered; nothing was started and it is still pending"
            );
            ExitCode::FAILURE
        }
        run::Ran::NotAsked { pending_id, why } => {
            eprintln!(
                "agentlife {command}: {pending_id}: the person was not asked ({why}); nothing was started and it is still pending"
            );
            ExitCode::FAILURE
        }
        run::Ran::Voided {
            pending_id,
            why,
            diffs,
        } => {
            eprintln!(
                "agentlife {command}: {pending_id} was voided and nobody was asked: {why}; nothing was started"
            );
            for d in diffs {
                eprintln!("  {d}");
            }
            ExitCode::FAILURE
        }
        run::Ran::NothingToStart => {
            println!("agentlife {command}: the plan starts nothing; nothing to ask about");
            ExitCode::SUCCESS
        }
    }
}

/// `agentlife pending`. Listing and showing only read. Discarding withdraws the request from the
/// shared `user-request` store; approving asks (agent list, then Hello) and runs on approval.
fn run_pending(action: PendingAction) -> ExitCode {
    let fail = |e: String| {
        eprintln!("agentlife pending: {e}");
        ExitCode::FAILURE
    };
    let home = match Home::from_env() {
        Ok(h) => h,
        Err(e) => return fail(e.to_string()),
    };
    match action {
        PendingAction::Prompt => {
            let (records, _) = pending::list(&home);
            // Oldest first; creating a newer one superseded the older ones, so normally one is open.
            let open: Vec<String> = records
                .iter()
                .filter(|r| r.is_open())
                .map(|r| r.pending_id.clone())
                .collect();
            if open.is_empty() {
                return ExitCode::SUCCESS;
            }
            approve_open(&home, &open)
        }
        PendingAction::List { json, all } => {
            let (records, problems) = pending::list(&home);
            let shown: Vec<_> = records.iter().filter(|r| all || r.is_open()).collect();
            if json {
                match serde_json::to_string_pretty(&shown) {
                    Ok(s) => println!("{s}"),
                    Err(e) => return fail(e.to_string()),
                }
            } else if shown.is_empty() {
                println!(
                    "no pending restores{}",
                    if all {
                        ""
                    } else {
                        " (--all includes closed ones)"
                    }
                );
            } else {
                for r in shown {
                    print!("{}", r.render_text());
                }
            }
            for (path, why) in &problems {
                eprintln!("agentlife pending: could not use {}: {why}", path.display());
            }
            ExitCode::SUCCESS
        }
        PendingAction::Show { id } => match pending::load(&home, &id) {
            Ok(r) => {
                print!("{}", r.render_text());
                println!(
                    "  frozen plan: {}",
                    pending::frozen_path(&home, &r).display()
                );
                ExitCode::SUCCESS
            }
            Err(e) => fail(e),
        },
        PendingAction::Discard { id } => {
            let mut backend = match consent::installed() {
                Ok(b) => b,
                Err(e) => return fail(e),
            };
            match pending::discard(&home, backend.as_mut(), &id, SystemClock.now()) {
                Ok(()) => {
                    println!("discarded {id}");
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            }
        }
        PendingAction::Approve { id } => approve_open(&home, &[id]),
    }
}

/// Asks about each pending request in turn (the agent list first, then Hello) and, on the first
/// approval, spends it and runs the plan. Stops there: that run changed what the others were about.
fn approve_open(home: &Home, ids: &[String]) -> ExitCode {
    let cfg = match Config::load(home, Layer::default()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("agentlife pending: {e}");
            return ExitCode::FAILURE;
        }
    };
    // The plan is rebuilt with the defaults: a request made with `--only` or `--priority` is a
    // different plan now and is voided unasked (re-run `restore` with the same flags instead).
    run_with_runtime(home, &cfg, &[], None, "pending", |rt| {
        let mut last: Option<Result<run::Ran, String>> = None;
        for id in ids {
            let r = run::approve_pending(rt, id);
            let done = !matches!(
                r,
                Ok(run::Ran::Cancelled { .. }
                    | run::Ran::StillPending { .. }
                    | run::Ran::NotAsked { .. }
                    | run::Ran::Voided { .. })
            );
            if !done {
                if let Ok(ran) = &r {
                    let _ = report_ran("pending", ran);
                }
            }
            last = Some(r);
            if done {
                break;
            }
        }
        last.unwrap_or(Ok(run::Ran::NothingToStart))
    })
}

/// The real preconditions a logon restore waits for.
struct SystemProbe {
    /// Where the message transport listens; `None` (Synapse with no `synapsed.json`) never holds.
    broker: Option<std::net::SocketAddr>,
    host_program: String,
}

impl task::Probe for SystemProbe {
    fn holds(&self, p: task::Precondition) -> bool {
        match p {
            task::Precondition::Network => std::process::Command::new("gh")
                .args(["api", "rate_limit"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success()),
            task::Precondition::Broker => self.broker.is_some_and(|addr| {
                std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(2))
                    .is_ok()
            }),
            task::Precondition::HostProgram => program_exists(&self.host_program),
        }
    }
    fn sleep(&self, d: std::time::Duration) {
        std::thread::sleep(d);
    }
}

/// A path that exists, or a bare name found on `PATH` (with `.exe` tried on Windows).
fn program_exists(program: &str) -> bool {
    let direct = std::path::Path::new(program);
    if direct.components().count() > 1 {
        return direct.is_file() || direct.with_extension("exe").is_file();
    }
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths)
            .any(|d| d.join(program).is_file() || d.join(format!("{program}.exe")).is_file())
    })
}

/// Waits (bounded by `network_wait_secs`) for what a logon restore needs. Not being ready is not an
/// error: the restore goes on and names what was missing, because a missing broker blocks only the
/// notice and a missing network only the approvals fetch.
fn wait_for_logon_preconditions() -> Result<(), String> {
    let home = Home::from_env().map_err(|e| e.to_string())?;
    let cfg = Config::load(&home, Layer::default()).map_err(|e| e.to_string())?;
    // The same switch as the notify path: `notify_transport` (default broker). With `synapse` the
    // probe reads the daemon's address from its home and never starts one.
    let broker = match agentlife::launcher_service::notify_transport(
        lane_restart::paths::home_dir().as_deref(),
    ) {
        lane_restart::notify::Transport::Broker => Some(*cfg.peers_addr),
        lane_restart::notify::Transport::Synapse => synapse::keystore::default_home()
            .ok()
            .and_then(|h| agentlife::peers::synapse_addr(&h)),
    };
    let probe = SystemProbe {
        broker,
        host_program: cfg.host_program.to_string(),
    };
    let report = task::wait_ready(
        &probe,
        std::time::Duration::from_secs(cfg.network_wait_secs),
        std::time::Duration::from_secs(5),
    );
    if report.ready() {
        eprintln!(
            "agentlife restore: ready after {} s",
            report.waited.as_secs()
        );
    } else {
        let names: Vec<_> = report.missing.iter().map(|p| p.name()).collect();
        eprintln!(
            "agentlife restore: not ready after {} s, missing: {}",
            report.waited.as_secs(),
            names.join(", ")
        );
    }
    Ok(())
}

/// `agentlife install-task`. Prints by default; `--register` / `--remove` call `schtasks`.
/// R16 and R17: reads the installed host and hook binary, changes nothing.
/// `agentlife worktrees --stale`: read-only; prints, never removes.
fn run_worktrees(json: bool, root: Option<String>, repos: Vec<String>) -> ExitCode {
    use agentlife::worktrees;
    let explicit: Vec<std::path::PathBuf> = repos.into_iter().map(Into::into).collect();
    let root = root.map_or_else(
        lane_state::platform::portfolio_root,
        std::path::PathBuf::from,
    );
    let (found, how) = worktrees::repos(&explicit, &root);
    let report = worktrees::scan(&worktrees::RealRunner, &found, how);
    if json {
        match serde_json::to_string_pretty(&report) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("agentlife worktrees: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        print!("{}", worktrees::render(&report));
    }
    ExitCode::SUCCESS
}

/// `agentlife launcher`: see `agentlife::launcher_service`. Exit 3 when another instance holds the
/// lock, 1 on any other refusal to start.
fn run_launcher(init_store: bool, once: bool) -> ExitCode {
    use agentlife::launcher_service as svc;
    use launcher::{service, Engine, FileStore};
    use std::time::Duration;

    let app = match app("launcher") {
        Ok(a) => a,
        Err(c) => return c,
    };
    let state_dir = state_dir_of(&app.cfg);
    // `lane-restart start` looks for the caller record in ITS state directory (the platform one),
    // whatever `lane_state_dir` the config names, so the record goes there.
    let record_dir = lane_restart::state_dir();
    let log_file = Home::from_env().ok().map(|h| h.root().join("launcher.log"));
    let log = |text: &str| {
        let line = format!(
            "{} agentlife launcher: {text}",
            chrono::Utc::now().to_rfc3339()
        );
        eprintln!("{line}");
        if let Some(p) = &log_file {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
            {
                let _ = writeln!(f, "{line}");
            }
        }
    };
    let fail = |text: String| {
        log(&text);
        ExitCode::FAILURE
    };

    let _lock = match svc::take_instance_lock(&state_dir) {
        Ok(Some(l)) => l,
        Ok(None) => {
            log("another instance holds the lock; exiting");
            return ExitCode::from(3);
        }
        Err(e) => return fail(format!("cannot take the instance lock: {e}")),
    };

    let store = FileStore {
        path: svc::store_path(&state_dir),
    };
    let table = SysinfoTable;
    let dir = svc::RegistryDirectory {
        registry: &app.registry,
        lane_state_dir: &state_dir,
        table: &table,
        wake_enabled: app.cfg.lazy_enabled,
    };
    let real_mech;
    let mech: &dyn launcher::Mechanism = if app.cfg.lazy_enabled {
        let wait = std::time::Duration::from_secs((svc::LAUNCH_WAIT_SECS - 10).max(1) as u64);
        match svc::LaneRestartMechanism::sibling(wait) {
            Ok(m) => {
                real_mech = m;
                &real_mech
            }
            Err(e) => return fail(format!("cannot locate lane-restart: {e}")),
        }
    } else {
        &svc::NoMechanism
    };
    let mut engine = match Engine::open(&store, mech, &dir, init_store) {
        Ok(e) => e,
        Err(e) => {
            return fail(format!(
                "{e} (the service never creates its store; run once with --init-store)"
            ))
        }
    };
    let now = || chrono::Utc::now().timestamp();
    match engine.recover(now()) {
        Ok(0) => {}
        Ok(n) => log(&format!(
            "resolved {n} request(s) left in flight by a crash"
        )),
        Err(e) => return fail(format!("cannot resolve in-flight requests: {e}")),
    }

    let home = match synapse::keystore::default_home() {
        Ok(h) => h,
        Err(e) => return fail(format!("no Synapse home (set SYNAPSE_HOME): {e}")),
    };
    let source = match svc::SynapseSource::connect(&home) {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    let peer_notify = svc::PeerNotify {
        state_dir: &state_dir,
    };
    let synapse_notify = svc::SynapseNotify { source: &source };
    let transport = svc::notify_transport(lane_restart::paths::home_dir().as_deref());
    let notify: &dyn service::Notify = match transport {
        lane_restart::notify::Transport::Broker => &peer_notify,
        lane_restart::notify::Transport::Synapse => &synapse_notify,
    };
    let record = svc::own_record();
    let mut holding = false;
    let mut failures = 0u32;
    log(&format!(
        "started, notifier-only, role {}",
        lane_restart::start::LAUNCHER_ROLE
    ));

    let exit = loop {
        // The caller record exists only while a valid session is held.
        if !holding {
            match source.session_ok() {
                Ok(()) => match &record {
                    Some(r) => match svc::write_caller_record(&record_dir, r) {
                        Ok(()) => holding = true,
                        Err(e) => log(&format!("cannot write the caller record: {e}")),
                    },
                    None => log("cannot read this process's start time; no caller record"),
                },
                Err(e) => {
                    log(&format!("no valid session: {e:?}"));
                    if once {
                        break ExitCode::FAILURE;
                    }
                    std::thread::sleep(Duration::from_secs(30));
                    continue;
                }
            }
        }
        let (relayed, queued) = svc::relay_outbox(&state_dir, &source);
        if relayed + queued > 0 {
            log(&format!(
                "notify outbox: relayed={relayed} still-queued={queued}"
            ));
        }
        let wait = if once { 0 } else { svc::POLL_WAIT_SECS };
        let rep = service::cycle(
            &mut engine,
            &source,
            notify,
            now(),
            svc::LAUNCH_WAIT_SECS,
            wait,
        );
        for note in &rep.notes {
            log(note);
        }
        for (text, reached) in &rep.alarms {
            log(&format!("alarm {text:?} reached={reached}"));
        }
        if rep.decided + rep.posted + rep.cleared > 0 {
            log(&format!(
                "cycle: decided={} posted={} cleared={}",
                rep.decided, rep.posted, rep.cleared
            ));
        }
        if rep.session_lost {
            if let Some(r) = &record {
                svc::remove_caller_record(&record_dir, r);
            }
            holding = false;
            log("session lost: caller record removed");
            if once {
                break ExitCode::FAILURE;
            }
            std::thread::sleep(Duration::from_secs(30));
            continue;
        }
        if once {
            break ExitCode::SUCCESS;
        }
        // A cycle that did not hold the poll must not spin. Repeated trouble (a daemon down, a
        // store that will not save) backs off 5, 10, 20, 40, then 60 seconds, and one clean
        // cycle resets it.
        let pause = if rep.notes.is_empty() {
            failures = 0;
            1
        } else {
            failures = failures.saturating_add(1);
            (5u64 << (failures - 1).min(4)).min(60)
        };
        std::thread::sleep(Duration::from_secs(pause));
    };
    if holding {
        if let Some(r) = &record {
            svc::remove_caller_record(&record_dir, r);
        }
    }
    exit
}

fn run_doctor(exe: Option<String>) -> ExitCode {
    use agentlife::doctor;
    let home = match Home::from_env() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("agentlife doctor: {e}");
            return ExitCode::FAILURE;
        }
    };
    let cfg = match Config::load(&home, Layer::default()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("agentlife doctor: {e}");
            return ExitCode::FAILURE;
        }
    };
    let resolve = |name: &str| agentlife::restore::resolve_program(name);
    let version_line = |p: &std::path::Path| -> Result<String, String> {
        let mut child = std::process::Command::new(p)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match child.try_wait().map_err(|e| e.to_string())? {
                Some(status) if status.success() => break,
                Some(status) => return Err(format!("exited with {status}")),
                None if std::time::Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("did not answer --version within 10 s".into());
                }
                None => std::thread::sleep(std::time::Duration::from_millis(50)),
            }
        }
        let mut text = String::new();
        if let Some(mut out) = child.stdout.take() {
            let _ = out.read_to_string(&mut text);
        }
        text.lines()
            .next()
            .map(str::to_string)
            .ok_or_else(|| "printed nothing".to_string())
    };
    let default_hook_exe = doctor::hook_exe();
    let hook_exe = exe.as_deref().unwrap_or(default_hook_exe.as_str());
    let lines = doctor::run(
        &cfg.host_program,
        hook_exe,
        agentlife::version(),
        &doctor::Probe {
            resolve: &resolve,
            version_line: &version_line,
        },
    );
    print!("{}", doctor::render(&lines));
    if doctor::all_ok(&lines) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn run_install_task(register: bool, remove: bool, exe: Option<String>) -> ExitCode {
    if !cfg!(windows) {
        return run_install_task_systemd(register, remove, exe);
    }
    let exe = match exe {
        Some(e) => e,
        None => match std::env::current_exe() {
            Ok(p) => p.display().to_string(),
            Err(e) => {
                eprintln!("agentlife install-task: cannot find this binary ({e}); pass --exe");
                return ExitCode::FAILURE;
            }
        },
    };
    let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(d), Ok(u)) if !d.is_empty() && !u.is_empty() => format!("{d}\\{u}"),
        (_, Ok(u)) if !u.is_empty() => u,
        _ => {
            eprintln!("agentlife install-task: cannot tell the user (USERNAME is unset)");
            return ExitCode::FAILURE;
        }
    };
    let mut failed = false;
    for t in [task::Trigger::Logon, task::Trigger::SessionUnlock] {
        if remove {
            failed |= !run_schtasks(&task::delete_args(t));
            continue;
        }
        let xml = task::task_xml(t, &user, &exe);
        if register {
            let path =
                std::env::temp_dir().join(format!("{}-{}.xml", t.task_name(), std::process::id()));
            if let Err(e) = std::fs::write(&path, task::utf16le_with_bom(&xml)) {
                eprintln!(
                    "agentlife install-task: cannot write {}: {e}",
                    path.display()
                );
                failed = true;
                continue;
            }
            failed |= !run_schtasks(&task::create_args(t, &path.display().to_string()));
            let _ = std::fs::remove_file(&path);
        } else {
            println!("# {}  ({})", t.task_name(), t.arguments());
            println!(
                "schtasks {}\n",
                task::create_args(t, "<file>.xml").join(" ")
            );
            println!("{xml}");
        }
    }
    if !register && !remove {
        println!(
            "# Nothing was changed. Save each XML as UTF-16 and run its schtasks line, or run"
        );
        println!("# `agentlife install-task --register`.");
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `agentlife install-task` off Windows (`docs/PORT-LINUX-PLAN.md`, P8): PRINTS the `systemd --user`
/// unit and the commands to install it. It never writes into the user's systemd directory or runs
/// `systemctl`: registering a unit that starts a restore at logon is a person's act (CireSnave
/// approves, the PM operates), and this path has not been run against a real systemd.
fn run_install_task_systemd(register: bool, remove: bool, exe: Option<String>) -> ExitCode {
    if register || remove {
        eprintln!(
            "agentlife install-task: --register/--remove exist only on Windows. On Linux this prints              the systemd --user unit and the commands; install it by hand."
        );
        return ExitCode::FAILURE;
    }
    let exe = match exe {
        Some(e) => e,
        None => match std::env::current_exe() {
            Ok(p) => p.display().to_string(),
            Err(e) => {
                eprintln!("agentlife install-task: cannot find this binary ({e}); pass --exe");
                return ExitCode::FAILURE;
            }
        },
    };
    for t in [task::Trigger::Logon, task::Trigger::SessionUnlock] {
        match task::systemd_unit(t, &exe) {
            Some(unit) => {
                let name = task::systemd_unit_name(t);
                println!("# {}  ({})", t.task_name(), t.arguments());
                println!("# save as ~/.config/systemd/user/{name}, then:");
                println!("#   systemctl --user daemon-reload");
                println!(
                    "#   systemctl --user enable {name}
"
                );
                println!("{unit}");
            }
            None => {
                println!("# {}  ({})", t.task_name(), t.arguments());
                println!(
                    "# No unit: systemd has no trigger for a session unlock (logind sends a D-Bus
                     # `Unlock` signal that a listener would have to catch). Run                      `agentlife pending --prompt` by hand, or from your desktop's own unlock hook.
"
                );
            }
        }
    }
    println!("# Nothing was changed (not verified against a real systemd).");
    ExitCode::SUCCESS
}

/// Runs `schtasks`, echoing what it said. `true` when it succeeded.
fn run_schtasks(args: &[String]) -> bool {
    match std::process::Command::new("schtasks").args(args).output() {
        Ok(out) => {
            print!("{}", String::from_utf8_lossy(&out.stdout));
            eprint!("{}", String::from_utf8_lossy(&out.stderr));
            if !out.status.success() {
                eprintln!(
                    "agentlife install-task: schtasks {} failed ({})",
                    args.join(" "),
                    out.status
                );
            }
            out.status.success()
        }
        Err(e) => {
            eprintln!("agentlife install-task: cannot run schtasks: {e}");
            false
        }
    }
}

fn all_records(app: &App, command: &str) -> Result<Vec<AgentRecord>, ExitCode> {
    app.registry.list().map(|l| l.records).map_err(|e| {
        eprintln!("agentlife {command}: {e}");
        ExitCode::FAILURE
    })
}

/// Who is calling, and the caller's own record when it is a registered agent.
fn who_is_calling(app: &App) -> (Caller, Option<AgentRecord>) {
    let caller = caller::detect(&RealEnv::new(), &app.procs);
    let rec = match &caller {
        Caller::Agent { id: Some(id) } => app.registry.get(id).ok().flatten(),
        _ => None,
    };
    (caller, rec)
}

/// `agentlife status --peers`: the Synapse roles. Read-only; claims `agentlife-cli` to ask.
fn run_status_peers(json: bool) -> ExitCode {
    if agentlife::launcher_service::notify_transport(lane_restart::paths::home_dir().as_deref())
        != lane_restart::notify::Transport::Synapse
    {
        eprintln!(
            "agentlife status --peers: notify_transport is not synapse; use `lane-restart status --peers` (the broker)"
        );
        return ExitCode::FAILURE;
    }
    let rows = synapse::keystore::default_home()
        .map_err(|e| format!("no Synapse home (set SYNAPSE_HOME): {e}"))
        .and_then(|h| agentlife::peers::SynapseMessenger::connect(&h))
        .and_then(|m| m.roles());
    match rows {
        Ok(rows) if json => match serde_json::to_string_pretty(&rows) {
            Ok(s) => {
                println!("{s}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("agentlife status: {e}");
                ExitCode::FAILURE
            }
        },
        Ok(rows) => {
            print!("{}", agentlife::peers::render_roles(&rows));
            ExitCode::SUCCESS
        }
        Err(e) => {
            // Unreachable is not "no roles".
            eprintln!("agentlife status --peers: cannot read the Synapse daemon: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_list(json: bool, all: bool) -> ExitCode {
    let app = match app("list") {
        Ok(a) => a,
        Err(c) => return c,
    };
    let listing = match app.registry.list() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("agentlife list: {e}");
            return ExitCode::FAILURE;
        }
    };
    let rows = list::rows(
        &listing.records,
        &SnapshotTable::capture(),
        all,
        &app.cfg,
        chrono::Utc::now(),
    );
    if json {
        println!("{}", list::render_json(&rows));
    } else {
        print!("{}", list::render_text(&rows));
    }
    for (path, why) in &listing.problems {
        eprintln!("agentlife list: could not use {}: {why}", path.display());
    }
    ExitCode::SUCCESS
}

/// `agentlife import-lane-state`: dry run unless `--write`; reads `.lane-state`, never writes it.
fn run_import(write: bool, json: bool, since: Option<String>, park: Vec<String>) -> ExitCode {
    use agentlife::import;
    let app = match app("import-lane-state") {
        Ok(a) => a,
        Err(c) => return c,
    };
    let fail = |e: String| {
        eprintln!("agentlife import-lane-state: {e}");
        ExitCode::FAILURE
    };
    let since = match since.as_deref().map(import::parse_since) {
        Some(Ok(d)) => d,
        Some(Err(e)) => return fail(e),
        None => chrono::Duration::hours(import::DEFAULT_SINCE_HOURS),
    };
    let dir = std::path::Path::new(&app.cfg.lane_state_dir);
    let (states, unparsed) = match import::read_states(dir) {
        Ok(r) => r,
        Err(e) => return fail(format!("cannot read {}: {e}", dir.display())),
    };
    let existing = match app.registry.list() {
        Ok(l) => l.records,
        Err(e) => return fail(e.to_string()),
    };
    let outcome = import::decide(&import::Inputs {
        states: &states,
        unparsed: &unparsed,
        existing: &existing,
        cfg: &app.cfg,
        now: chrono::Utc::now(),
        since,
        park: &park,
        cwd_exists: &|p| std::path::Path::new(p).is_dir(),
    });
    let written = if write {
        match import::write(&app.registry, &outcome) {
            Ok(n) => n,
            Err(e) => return fail(e.to_string()),
        }
    } else {
        0
    };
    if write {
        for imp in &outcome.imports {
            let _ = app.journal.append(
                "imported",
                Some(imp.record.agent_id.as_str()),
                serde_json::json!({
                    "role": imp.role,
                    "name": imp.record.name,
                    "cwd": imp.record.launch_cwd,
                    "permission_mode": imp.record.permission_mode,
                    "intent": imp.record.intent,
                }),
            );
        }
    }
    if json {
        let v = serde_json::json!({
            "wrote": write,
            "written": written,
            "since_hours": since.num_hours(),
            "imports": outcome.imports.iter().map(|i| serde_json::json!({
                "role": i.role,
                "agent_id": i.record.agent_id,
                "name": i.record.name,
                "cwd": i.record.launch_cwd,
                "permission_mode": i.record.permission_mode,
                "parked": !matches!(i.record.intent, agentlife::registry::Intent::Wanted),
            })).collect::<Vec<_>>(),
            "skipped": outcome.skipped,
            "unmatched_park": outcome.unmatched_park,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".into())
        );
    } else {
        println!(
            "{} of {} files in {}{}",
            outcome.imports.len(),
            states.len() + unparsed.len(),
            dir.display(),
            if write {
                ""
            } else {
                "  (dry run: nothing written; --write to import)"
            }
        );
        for i in &outcome.imports {
            println!(
                "{} {:<26} mode={:<18} {}{}",
                if write { "imported" } else { "would import" },
                i.record.name.as_deref().unwrap_or("-"),
                i.record
                    .permission_mode
                    .as_ref()
                    .map(|m| m.as_str())
                    .unwrap_or("-"),
                i.record.launch_cwd,
                if matches!(i.record.intent, agentlife::registry::Intent::Wanted) {
                    ""
                } else {
                    "  [parked]"
                }
            );
        }
        for s in &outcome.skipped {
            println!("skipped  {:<26} {:?}: {}", s.role, s.reason, s.detail);
        }
        for p in &outcome.unmatched_park {
            println!("--park {p:?} matched no imported agent");
        }
    }
    ExitCode::SUCCESS
}

fn run_reconcile(dry_run: bool, json: bool) -> ExitCode {
    let app = match app("reconcile") {
        Ok(a) => a,
        Err(c) => return c,
    };
    let table = SnapshotTable::capture();
    let boot = intent::boot_time();
    let facts = intent::Facts {
        table: &table,
        boot,
        shutdown_start: EventLogMarker.latest_before(boot),
    };
    let out = match intent::reconcile(&app.registry, &app.journal, &facts, dry_run) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("agentlife reconcile: {e}");
            return ExitCode::FAILURE;
        }
    };
    if json {
        let v: Vec<_> = out
            .iter()
            .map(|r| {
                serde_json::json!({
                    "agent_id": r.agent_id,
                    "name": r.name,
                    "verdict": r.verdict.describe(),
                    "persisted": r.persisted,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "[]".into())
        );
    } else {
        println!(
            "boot {}; shutdown before it: {}{}",
            boot.format("%Y-%m-%d %H:%MZ"),
            facts.shutdown_start.map_or("unknown".to_string(), |t| t
                .format("%Y-%m-%d %H:%MZ")
                .to_string()),
            if dry_run {
                "  (dry run: nothing written)"
            } else {
                ""
            }
        );
        for r in &out {
            println!(
                "{:<26} {:<28} {}{}",
                r.name.as_deref().unwrap_or("-"),
                r.agent_id,
                r.verdict.describe(),
                if r.persisted { "  [written down]" } else { "" }
            );
        }
        println!("{} agents", out.len());
    }
    ExitCode::SUCCESS
}

enum Mark {
    Pin,
    Unpin,
}

fn run_mark(selector: &str, mark: Mark) -> ExitCode {
    let name = if matches!(mark, Mark::Pin) {
        "pin"
    } else {
        "unpin"
    };
    let app = match app(name) {
        Ok(a) => a,
        Err(c) => return c,
    };
    let records = match all_records(&app, name) {
        Ok(r) => r,
        Err(c) => return c,
    };
    let target = match select::resolve(selector, &records) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("agentlife {name}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (caller, caller_rec) = who_is_calling(&app);
    if let Err(why) = marks::authorize(&caller, caller_rec.as_ref(), &target.agent_id, &app.cfg) {
        eprintln!("agentlife {name}: refused: {why}");
        return ExitCode::FAILURE;
    }
    let result = match mark {
        Mark::Pin => marks::pin(
            &app.registry,
            &app.journal,
            &target.agent_id,
            &caller,
            chrono::Utc::now(),
        ),
        Mark::Unpin => marks::unpin(&app.registry, &app.journal, &target.agent_id, &caller),
    };
    report(name, &target.agent_id.to_string(), result)
}

fn run_waiting(selector: Option<&str>, note: Option<String>, clear: bool) -> ExitCode {
    let app = match app("waiting") {
        Ok(a) => a,
        Err(c) => return c,
    };
    let (caller, caller_rec) = who_is_calling(&app);
    let records = match all_records(&app, "waiting") {
        Ok(r) => r,
        Err(c) => return c,
    };
    let target_id = match selector {
        Some(s) => match select::resolve(s, &records) {
            Ok(t) => t.agent_id.clone(),
            Err(e) => {
                eprintln!("agentlife waiting: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => match &caller {
            Caller::Agent { id: Some(id) } => id.clone(),
            _ => {
                eprintln!(
                    "agentlife waiting: no --agent given, and this command is not running inside a registered agent session"
                );
                return ExitCode::FAILURE;
            }
        },
    };
    if let Err(why) = marks::authorize(&caller, caller_rec.as_ref(), &target_id, &app.cfg) {
        eprintln!("agentlife waiting: refused: {why}");
        return ExitCode::FAILURE;
    }
    let result = if clear {
        marks::clear_waiting(&app.registry, &app.journal, &target_id, &caller)
    } else {
        marks::set_waiting(
            &app.registry,
            &app.journal,
            &target_id,
            note,
            &caller,
            chrono::Utc::now(),
        )
    };
    report("waiting", &target_id.to_string(), result)
}

fn run_close(
    selector: &str,
    how: ClosedHow,
    confirm: Option<&str>,
    yes: bool,
    timeout: Option<u64>,
    soft: bool,
) -> ExitCode {
    let name = if how == ClosedHow::Parked {
        "park"
    } else {
        "stop"
    };
    let app = match app(name) {
        Ok(a) => a,
        Err(c) => return c,
    };
    let records = match all_records(&app, name) {
        Ok(r) => r,
        Err(c) => return c,
    };
    let target = match select::resolve(selector, &records) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("agentlife {name}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (caller, caller_rec) = who_is_calling(&app);
    let table = SnapshotTable::capture();
    if list::liveness(target, &table).0 != list::Liveness::Stopped {
        return run_down(
            &app,
            name,
            target,
            how,
            soft,
            confirm,
            yes,
            timeout,
            &caller,
            caller_rec.as_ref(),
        );
    }
    if soft {
        eprintln!(
            "agentlife {name}: --soft applies to ending a RUNNING session; this agent is not running, so nothing would be recorded"
        );
        return ExitCode::FAILURE;
    }
    let result = control::close(
        &app.registry,
        &app.journal,
        &table,
        &app.cfg,
        &caller,
        caller_rec.as_ref(),
        &target.agent_id,
        how,
        confirm,
        chrono::Utc::now(),
    );
    control_report(name, &target.agent_id.to_string(), result)
}

/// A RUNNING agent: the graceful stop (`down::stop_running`). The only place a process is ended.
#[allow(clippy::too_many_arguments)]
fn run_down(
    app: &App,
    command: &str,
    target: &AgentRecord,
    how: ClosedHow,
    soft: bool,
    confirm: Option<&str>,
    yes: bool,
    timeout: Option<u64>,
    caller: &Caller,
    caller_rec: Option<&AgentRecord>,
) -> ExitCode {
    let table = SnapshotTable::capture();
    let broker = Broker::new(*app.cfg.peers_addr);
    let synapse_messenger;
    let messenger: &dyn agentlife::peers::Messenger =
        match agentlife::launcher_service::notify_transport(
            lane_restart::paths::home_dir().as_deref(),
        ) {
            lane_restart::notify::Transport::Broker => &broker,
            lane_restart::notify::Transport::Synapse => {
                let connected = synapse::keystore::default_home()
                    .map_err(|e| format!("no Synapse home (set SYNAPSE_HOME): {e}"))
                    .and_then(|h| agentlife::peers::SynapseMessenger::connect(&h));
                match connected {
                    Ok(m) => {
                        synapse_messenger = m;
                        &synapse_messenger
                    }
                    Err(e) => {
                        eprintln!("agentlife {command}: cannot reach the Synapse daemon: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            }
        };
    // One snapshot serves both the peer-to-lane join and the live-shell check.
    let tree = SnapshotParents::capture();
    let lane_state_dir = std::path::PathBuf::from(&app.cfg.lane_state_dir);
    let facts = down::FsFacts {
        lane_state_dir: &lane_state_dir,
    };
    let sleeper = |d: std::time::Duration| std::thread::sleep(d);
    let outcome = down::stop_running(
        &down::Deps {
            registry: &app.registry,
            journal: &app.journal,
            cfg: &app.cfg,
            table: &table,
            messenger,
            parents: &tree,
            tree: &tree,
            facts: &facts,
            terminator: &down::SysinfoTerminator,
            clock: &SystemClock,
            sleep: &sleeper,
        },
        &down::Request {
            target: &target.agent_id,
            how,
            kind: if soft {
                lane_restart::spec::StopKind::Soft
            } else {
                lane_restart::spec::StopKind::Hard
            },
            confirm,
            yes,
            timeout: std::time::Duration::from_secs(timeout.unwrap_or(app.cfg.down_timeout_secs)),
            poll: std::time::Duration::from_secs(app.cfg.down_poll_secs),
            caller,
            caller_rec,
        },
    );
    match outcome {
        Ok(down::Outcome::DryRun(plan)) => {
            for line in plan {
                println!("agentlife {command}: {line}");
            }
            // Distinct from success and from refusal, so a script can tell "only a plan" apart.
            ExitCode::from(2)
        }
        Ok(down::Outcome::Stopped { pid, waited }) => {
            println!(
                "agentlife {command}: stopped {} (pid {pid}) after {}s; recorded {}",
                target.name.as_deref().unwrap_or(target.agent_id.as_str()),
                waited.as_secs(),
                if how == ClosedHow::Parked {
                    "parked"
                } else {
                    "stopped"
                }
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("agentlife {command}: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_unpark(selector: &str) -> ExitCode {
    let app = match app("unpark") {
        Ok(a) => a,
        Err(c) => return c,
    };
    let records = match all_records(&app, "unpark") {
        Ok(r) => r,
        Err(c) => return c,
    };
    let target = match select::resolve(selector, &records) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("agentlife unpark: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (caller, caller_rec) = who_is_calling(&app);
    let result = control::unpark_no_start(
        &app.registry,
        &app.journal,
        &app.cfg,
        &caller,
        caller_rec.as_ref(),
        &target.agent_id,
    );
    control_report("unpark", &target.agent_id.to_string(), result)
}

fn report(command: &str, id: &str, result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => {
            println!("agentlife {command}: done for {id}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("agentlife {command}: {e}");
            ExitCode::FAILURE
        }
    }
}

fn control_report(command: &str, id: &str, result: Result<(), ControlError>) -> ExitCode {
    match result {
        Ok(()) => {
            println!("agentlife {command}: done for {id}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("agentlife {command}: {e}");
            ExitCode::FAILURE
        }
    }
}
