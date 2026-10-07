// SPDX-License-Identifier: MIT OR Apache-2.0
//! `agentlife`: agent lifecycle control. See `docs/MILESTONES.md`.

use agentlife::cli::{self, Command};
use agentlife::clock::SystemClock;
use agentlife::home::Home;
use agentlife::hook::{self, Ctx, RealEnv};
use agentlife::identity::{ProcessTable, SnapshotTable, SysinfoTable};
use agentlife::journal::Journal;
use agentlife::list;
use agentlife::procindex::ProcIndex;
use agentlife::registry::Registry;
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

fn run_list(json: bool, all: bool) -> ExitCode {
    let home = match Home::from_env() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("agentlife list: {e}");
            return ExitCode::FAILURE;
        }
    };
    let listing = match Registry::new(home.agents_dir()).list() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("agentlife list: {e}");
            return ExitCode::FAILURE;
        }
    };
    let rows = list::rows(&listing.records, &SnapshotTable::capture(), all);
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
