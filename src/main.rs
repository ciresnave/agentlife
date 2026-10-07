// SPDX-License-Identifier: MIT OR Apache-2.0
//! `agentlife`: agent lifecycle control. See `docs/MILESTONES.md`.

use agentlife::caller::{self, Caller};
use agentlife::cli::{self, Command};
use agentlife::clock::SystemClock;
use agentlife::config::{Config, Layer};
use agentlife::control::{self, ControlError};
use agentlife::home::Home;
use agentlife::hook::{self, Ctx, RealEnv};
use agentlife::identity::{ProcessTable, SnapshotTable, SysinfoTable};
use agentlife::intent::{self, EventLogMarker, ShutdownMarker};
use agentlife::journal::Journal;
use agentlife::list;
use agentlife::marks;
use agentlife::procindex::ProcIndex;
use agentlife::registry::{AgentRecord, ClosedHow, Registry};
use agentlife::select;
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
        Ok(Command::Reconcile { dry_run, json }) => run_reconcile(dry_run, json),
        Ok(Command::Pin { agent }) => run_mark(&agent, Mark::Pin),
        Ok(Command::Unpin { agent }) => run_mark(&agent, Mark::Unpin),
        Ok(Command::Waiting { agent, note, clear }) => run_waiting(agent.as_deref(), note, clear),
        Ok(Command::Park { agent, confirm }) => {
            run_close(&agent, ClosedHow::Parked, confirm.as_deref())
        }
        Ok(Command::Stop { agent, confirm }) => {
            run_close(&agent, ClosedHow::Exited, confirm.as_deref())
        }
        Ok(Command::Unpark { agent, .. }) => run_unpark(&agent),
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
        outcome.log_line(event, chrono::Utc::now()),
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

fn run_close(selector: &str, how: ClosedHow, confirm: Option<&str>) -> ExitCode {
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
    let result = control::close(
        &app.registry,
        &app.journal,
        &SnapshotTable::capture(),
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
