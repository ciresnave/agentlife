// SPDX-License-Identifier: MIT OR Apache-2.0

//! CLI entry point. All the actual decision logic lives in the library
//! (`authorize.rs`, `facts.rs`, `state.rs`, `log.rs`) so it can be unit
//! tested without a real process. This file only: parses argv, wires the
//! real `SystemFacts`, calls `authorize::decide`, and - only if it comes
//! back `Ok` with `will_act: true` - performs the kill and the relaunch.

use lane_restart::authorize::{self, Target};
use lane_restart::facts::SysinfoFacts;
use lane_restart::lane_state_writer::{self, RealParentProcess};
use lane_restart::lane_stop;
use lane_restart::log;
use lane_restart::relaunch;
use lane_restart::start;
use lane_restart::tab_close;
use std::path::PathBuf;
use std::process::ExitCode;

/// `C:/Projects/.lane-state` - RESTART-TOOL-DESIGN.md §7. Not configurable
/// via CLI on purpose: a caller-supplied state directory would defeat the
/// whole point of a fixed, portfolio-wide location every lane and the PM
/// agree on.
fn state_dir() -> PathBuf {
    lane_restart::state_dir()
}

fn log_path() -> PathBuf {
    state_dir().join("restart.log")
}

fn claude_config_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs_home().map(|h| h.join(".claude")))
        .expect("could not resolve the Claude Code config directory")
}

fn dirs_home() -> Option<PathBuf> {
    lane_restart::paths::home_dir()
}

#[derive(Debug)]
struct Args {
    role: String,
    target_self: bool,
    dry_run: bool,
    confirmed: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum ArgError {
    MissingRole,
    RoleRequired,
    Unknown(String),
    MissingChildArgv,
    MissingValue(&'static str),
}

impl std::fmt::Display for ArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArgError::MissingRole => write!(f, "--role requires a value"),
            ArgError::RoleRequired => write!(f, "--role <name> is required"),
            ArgError::Unknown(a) => write!(f, "unrecognised argument: {a}"),
            ArgError::MissingChildArgv => {
                write!(f, "expected `-- <child argv...>` after --role <name>")
            }
            ArgError::MissingValue(flag) => write!(f, "{flag} requires a value"),
        }
    }
}

/// ⚠️ `--self` DEFAULTS TO FALSE. Omitting it means "restart a different
/// lane" - the SAFER default is the one that requires `--yes` to act for
/// real, not the one that acts unconditionally. A caller who wants to
/// restart their own session must say so explicitly.
fn parse_args<I: IntoIterator<Item = String>>(argv: I) -> Result<Args, ArgError> {
    let mut role: Option<String> = None;
    let mut target_self = false;
    let mut dry_run = false;
    let mut confirmed = false;
    let mut iter = argv.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--role" => role = Some(iter.next().ok_or(ArgError::MissingRole)?),
            "--self" => target_self = true,
            "--dry-run" => dry_run = true,
            "--yes" => confirmed = true,
            other => return Err(ArgError::Unknown(other.to_string())),
        }
    }
    Ok(Args {
        role: role.ok_or(ArgError::RoleRequired)?,
        target_self,
        dry_run,
        confirmed,
    })
}

/// `lane-restart state <event>` - the hook command. Handled separately from
/// `parse_args` because its shape (a positional subcommand, then a single
/// positional event name) doesn't fit the restart CLI's own flags at all,
/// and mixing the two would make either grammar harder to read.
fn run_state_hook(event: &str) -> ExitCode {
    let started = std::time::Instant::now();
    let my_pid = std::process::id();
    let lane_role_env = std::env::var("LANE_ROLE").ok();
    let mut stdin = std::io::stdin();
    let lookup = RealParentProcess::new();
    let result = lane_state_writer::run(
        &state_dir(),
        event,
        my_pid,
        &lookup,
        lane_role_env.as_deref(),
        &mut stdin,
    );
    if let Some(note) = lane_state_writer::slow_note(event, started.elapsed(), lookup.full_scans())
    {
        eprintln!("{note}");
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lane-restart state {event}: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `lane-restart assert-idle [--role <name>]` - run by a lane itself, right
/// after writing HANDOFF. Separate from `parse_args` for the same reason
/// `state` is: a positional subcommand, not the restart CLI's own flags.
fn run_assert_idle_cmd(role_override: Option<String>) -> ExitCode {
    let my_pid = std::process::id();
    let lane_role_env = role_override.or_else(|| std::env::var("LANE_ROLE").ok());
    let lint_role = lane_role_env.clone();
    let cwd = match std::env::current_dir() {
        Ok(p) => p.to_string_lossy().to_string(),
        Err(e) => {
            eprintln!("lane-restart assert-idle: could not read the current directory: {e}");
            return ExitCode::FAILURE;
        }
    };
    match lane_state_writer::run_assert_idle(
        &state_dir(),
        my_pid,
        &RealParentProcess::new(),
        lane_role_env.as_deref(),
        &cwd,
    ) {
        Ok(()) => {
            // A warning, never a refusal: a restart is mandatory at a task boundary. The role is
            // the same one assert-idle resolved: --role, then LANE_ROLE, then the cwd's leaf.
            let role = lint_role.unwrap_or_else(|| {
                std::path::Path::new(&cwd)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
            if let Some(w) = lane_restart::handoff_lint::check(&cwd, &role) {
                eprintln!("lane-restart assert-idle: {w}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("lane-restart assert-idle: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `lane-restart host --role <role> -- <child argv...>` - RESTART-TOOL-
/// DESIGN.md §12.5. Runs INSIDE the relaunched session's own terminal tab
/// (launched by `spawn_relaunch`, below) and hosts `claude` in a ConPTY
/// this process owns.
fn run_host_cmd(role: &str, child_argv: &[String]) -> ExitCode {
    match lane_restart::host::run(role, child_argv) {
        Ok(outcome) => match outcome.child_exit_code {
            Some(0) | None => ExitCode::SUCCESS,
            Some(_) => ExitCode::FAILURE,
        },
        Err(e) => {
            eprintln!("lane-restart host: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Parses `--role <name> -- <child argv...>` for the `host` subcommand - a
/// positional subcommand with its own `--` separator, not `parse_args`'s
/// restart-CLI flag grammar.
fn parse_host_args(rest: &[String]) -> Result<(String, Vec<String>), ArgError> {
    let mut role: Option<String> = None;
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--role" => role = Some(iter.next().ok_or(ArgError::MissingRole)?.clone()),
            "--" => {
                let child_argv: Vec<String> = iter.cloned().collect();
                if child_argv.is_empty() {
                    return Err(ArgError::MissingChildArgv);
                }
                return Ok((role.ok_or(ArgError::RoleRequired)?, child_argv));
            }
            other => return Err(ArgError::Unknown(other.to_string())),
        }
    }
    Err(ArgError::MissingChildArgv)
}

/// `lane-restart --version` - RESTART-TOOL-DESIGN.md §12.9: the crate
/// version, that it embeds no approvals, and where it would fetch them
/// from. No network: `lane-restart approvals` does the fetch.
fn print_version() {
    use lane_restart::approvals;
    println!("lane-restart {}", env!("CARGO_PKG_VERSION"));
    println!("embedded approvals: none (by design - RESTART-TOOL-DESIGN.md §12.2)");
    match dirs_home() {
        Some(home) => match approvals::load_config(&approvals::config_path(&home)) {
            Ok(c) => println!(
                "approvals source: {}:{} (default branch), notify role: {}",
                c.approvals.repo, c.approvals.path, c.notify_role
            ),
            Err(e) => println!("approvals source: {e}"),
        },
        None => println!("approvals source: no home directory"),
    }
}

/// `lane-restart approvals [--role <name>]` - fetches the configured
/// approvals exactly as `host` would and prints every active one (id,
/// content hash, roles, expiry) and every refused one with its reason.
/// With `--role`, marks which apply to that role right now. Exit 1 if
/// nothing could be loaded at all.
fn run_approvals_cmd(role: Option<String>) -> ExitCode {
    use lane_restart::approvals;
    let Some(home) = dirs_home() else {
        eprintln!("lane-restart approvals: no home directory");
        return ExitCode::FAILURE;
    };
    let (_, loaded) = approvals::load_configured(&home, &approvals::GhReader::default());
    println!("approvals: {}", loaded.summary());
    let now = chrono::Utc::now();
    for (h, hash) in &loaded.handlers {
        let applies = match &role {
            Some(r) if lane_restart::handlers::is_active(h, r, now) => format!(" [applies to {r}]"),
            Some(r) => format!(" [NOT active for {r}]"),
            None => String::new(),
        };
        let expiry = h
            .expires_at
            .map_or_else(|| "never".to_string(), |e| e.to_rfc3339());
        println!(
            "  active {} {hash} roles={:?} expires={expiry}{applies}",
            h.id, h.scope.roles
        );
    }
    for (name, reason) in &loaded.refused {
        println!("  REFUSED {name}: {reason}");
    }
    if loaded.error.is_some() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `lane-restart status [--json]` - RESTART-TOOL-DESIGN.md 14.2 / 14.10. Read-only: for every
/// role with a lane state or a launch spec, whether it is running, suspended, crashed or never
/// run, and whether it can be started again from its spec. Liveness is the process table's
/// (pid and start time), never a file's age.
fn run_status_cmd(json: bool) -> ExitCode {
    use lane_restart::spec;
    let facts = SysinfoFacts::new(claude_config_dir());
    let report = spec::status_all(&state_dir(), &facts);
    if json {
        // The JSON shape is unchanged; the disk line goes to stderr so a reader of stdout is not
        // broken by it.
        eprintln!("{}", lane_restart::disk::status_line(&state_dir()));
        match serde_json::to_string_pretty(&report) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("lane-restart status: {e}");
                return ExitCode::FAILURE;
            }
        }
        return ExitCode::SUCCESS;
    }
    println!("{}", lane_restart::disk::status_line(&state_dir()));
    for r in &report.roles {
        println!(
            "{}\t{:?}\tspec={:?}\tstartable={}{}",
            r.role,
            r.state,
            r.spec,
            r.startable,
            r.detail
                .as_deref()
                .map(|d| format!("\t{d}"))
                .unwrap_or_default()
        );
    }
    let cov = &report.coverage;
    println!(
        "launch specs: {} of {} role(s) have a complete one{}",
        cov.complete,
        cov.total,
        if cov.gaps.is_empty() {
            ""
        } else {
            "; without:"
        }
    );
    for g in &cov.gaps {
        println!(
            "  {} role(s): {}: {}",
            g.roles.len(),
            g.why,
            g.roles.join(", ")
        );
    }
    if !report.skipped.is_empty() {
        println!(
            "({} other file(s) in the state directory are neither a lane state nor a launch spec: {})",
            report.skipped.len(),
            report.skipped.join(", ")
        );
    }
    ExitCode::SUCCESS
}

#[derive(Debug, PartialEq, Eq)]
struct StartArgs {
    role: String,
    reason: String,
    model: Option<String>,
    dry_run: bool,
}

/// `start --role R --reason <text> [--model sonnet|haiku] [--dry-run]`. A missing `--reason` is
/// not an argument error: it reaches `start::validate_reason`, so there is one message for it.
fn parse_start_args(rest: &[String]) -> Result<StartArgs, ArgError> {
    let mut role = None;
    let mut reason = String::new();
    let mut model = None;
    let mut dry_run = false;
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--role" => role = Some(iter.next().ok_or(ArgError::MissingRole)?.clone()),
            "--reason" => {
                reason = iter
                    .next()
                    .ok_or(ArgError::MissingValue("--reason"))?
                    .clone()
            }
            "--model" => {
                model = Some(
                    iter.next()
                        .ok_or(ArgError::MissingValue("--model"))?
                        .clone(),
                )
            }
            "--dry-run" => dry_run = true,
            other => return Err(ArgError::Unknown(other.to_string())),
        }
    }
    Ok(StartArgs {
        role: role.ok_or(ArgError::RoleRequired)?,
        reason,
        model,
        dry_run,
    })
}

/// The disk floor (`disk.rs`), checked before any launch. A refusal is printed as `kind: refused -
/// ...` and the caller exits 1 and logs it; an unreadable probe is a warning, not a refusal.
fn disk_gate(kind: &str) -> Result<(), lane_restart::disk::Refusal> {
    match lane_restart::disk::guard(&state_dir()) {
        Ok(lane_restart::disk::GuardOk::Free(_)) => Ok(()),
        Ok(lane_restart::disk::GuardOk::Unknown(why)) => {
            eprintln!("lane-restart {kind}: warning - free disk space was not checked: {why}");
            Ok(())
        }
        Err(refusal) => {
            eprintln!("lane-restart {kind}: refused - {refusal}");
            Err(refusal)
        }
    }
}

/// `specs import [--role R] [--dry-run] [--verbose]` -> `(role, dry_run, verbose)`.
fn parse_specs_import_args(rest: &[String]) -> Result<(Option<String>, bool, bool), ArgError> {
    let mut role = None;
    let mut dry_run = false;
    let mut verbose = false;
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--role" => role = Some(iter.next().ok_or(ArgError::MissingRole)?.clone()),
            "--dry-run" => dry_run = true,
            "--verbose" => verbose = true,
            other => return Err(ArgError::Unknown(other.to_string())),
        }
    }
    Ok((role, dry_run, verbose))
}

/// `specs retire --role R [--role R]... [--dry-run]` -> `(roles, dry_run)`.
fn parse_specs_retire_args(rest: &[String]) -> Result<(Vec<String>, bool), ArgError> {
    let mut roles = Vec::new();
    let mut dry_run = false;
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--role" => roles.push(iter.next().ok_or(ArgError::MissingRole)?.clone()),
            "--dry-run" => dry_run = true,
            other => return Err(ArgError::Unknown(other.to_string())),
        }
    }
    if roles.is_empty() {
        return Err(ArgError::RoleRequired);
    }
    Ok((roles, dry_run))
}

/// The role whose `claude` session is running this command, verified by pid and start time
/// (RESTART-TOOL-DESIGN.md 14.8), never by `LANE_ROLE`.
fn verified_caller(facts: &SysinfoFacts) -> Result<String, start::CallerError> {
    let pid =
        lane_state_writer::claude_parent_pid(std::process::id(), &RealParentProcess::new()).ok();
    start::caller_role(&state_dir(), pid, facts)
}

/// `lane-restart start --role R --reason <text> [--model sonnet|haiku] [--dry-run]` - 14.5.
fn run_start_cmd(args: StartArgs) -> ExitCode {
    let facts = SysinfoFacts::new(claude_config_dir());
    let caller = verified_caller(&facts);
    let caller_role = caller.as_ref().ok().cloned();
    let requested_by = caller_role
        .clone()
        .unwrap_or_else(|| "unverified".to_string());
    let reason_for_log: String = args
        .reason
        .trim()
        .chars()
        .take(start::REASON_MAX_CHARS)
        .collect();
    let ledger = |model: Option<&str>, outcome: &str, acted: bool| {
        log_ledger(
            &requested_by,
            &args.role,
            "start",
            caller_role.as_deref(),
            model,
            Some(&reason_for_log),
            outcome,
            acted,
        );
    };
    let req = start::StartRequest {
        role: &args.role,
        reason: &args.reason,
        model: args.model.as_deref(),
    };
    let plan = match start::plan_start(&state_dir(), &req, caller, &facts) {
        Ok(p) => p,
        Err(refusal) => {
            eprintln!("lane-restart start: refused - {refusal}");
            ledger(None, &format!("refused: {refusal}"), false);
            return ExitCode::FAILURE;
        }
    };
    if let Err(refusal) = disk_gate("start") {
        ledger(None, &format!("refused: {refusal}"), false);
        return ExitCode::FAILURE;
    }
    let pin_note = plan
        .pinned_from
        .as_deref()
        .map(|was| format!(" model-pinned:{was}"))
        .unwrap_or_default();
    if args.dry_run {
        let cmdline: Vec<String> = plan
            .argv
            .iter()
            .map(|a| {
                if a.contains(' ') {
                    format!("{a:?}")
                } else {
                    a.clone()
                }
            })
            .collect();
        println!(
            "lane-restart start: DRY RUN - would launch {} via wt.exe (conhost.exe fallback), \
             hosted by `lane-restart host --role {} -- {}`, in {}{pin_note}",
            plan.launch.role,
            plan.launch.role,
            cmdline.join(" "),
            plan.launch.cwd
        );
        ledger(
            Some(&plan.model),
            &format!("dry run - no action taken{pin_note}"),
            false,
        );
        return ExitCode::SUCCESS;
    }
    eprintln!(
        "lane-restart start: launching {} on {}...",
        args.role, plan.model
    );
    let state_dir_path = state_dir();
    let state_reader = relaunch::RealStateReader {
        state_dir: &state_dir_path,
    };
    let role_for_notice = plan.launch.role.clone();
    let mut on_awaiting = || print_status_json("awaiting_confirmation", &role_for_notice);
    let result = start::execute_start(
        &plan,
        &facts,
        &state_reader,
        &lane_restart::spec::DirSpecStore {
            dir: state_dir_path.clone(),
        },
        &lane_restart::launch::LivenessTiming::default(),
        &relaunch::spawn_launch,
        &mut std::thread::sleep,
        &mut on_awaiting,
    );
    match result {
        Ok(done) => {
            let warn = done
                .spec_warning
                .as_deref()
                .map(|w| format!("; the launch spec could not be marked running: {w}"))
                .unwrap_or_default();
            let (what, exit) = match done.outcome {
                relaunch::RelaunchOutcome::Relaunched => (
                    "started a fresh session (continuity via HANDOFF only)",
                    ExitCode::SUCCESS,
                ),
                relaunch::RelaunchOutcome::AwaitingConfirmation => (
                    "started; awaiting human confirmation at the dev-channels dialog",
                    ExitCode::from(2),
                ),
            };
            ledger(Some(&plan.model), &format!("{what}{pin_note}{warn}"), true);
            exit
        }
        Err(e) => {
            eprintln!("lane-restart start: {e}");
            ledger(
                Some(&plan.model),
                &format!("action failed: {e}{pin_note}"),
                false,
            );
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct StopArgs {
    role: String,
    reason: String,
    target_self: bool,
    dry_run: bool,
    confirmed: bool,
}

/// `stop --role R --reason <text> [--self] [--dry-run] [--yes]`. As with `start`, a missing
/// `--reason` reaches `start::validate_reason`, so there is one message for it.
fn parse_stop_args(rest: &[String]) -> Result<StopArgs, ArgError> {
    let mut role = None;
    let mut reason = String::new();
    let (mut target_self, mut dry_run, mut confirmed) = (false, false, false);
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--role" => role = Some(iter.next().ok_or(ArgError::MissingRole)?.clone()),
            "--reason" => {
                reason = iter
                    .next()
                    .ok_or(ArgError::MissingValue("--reason"))?
                    .clone()
            }
            "--self" => target_self = true,
            "--dry-run" => dry_run = true,
            "--yes" => confirmed = true,
            other => return Err(ArgError::Unknown(other.to_string())),
        }
    }
    Ok(StopArgs {
        role: role.ok_or(ArgError::RoleRequired)?,
        reason,
        target_self,
        dry_run,
        confirmed,
    })
}

/// `lane-restart stop --role R --reason <text> [--self] [--dry-run] [--yes]` - 14.3. Suspends a
/// lane: the complete spec first, then the verified kill, then the spec marked `suspended`.
/// Exit codes: 0 stopped and suspended, 1 refused or failed, 4 killed but the `suspended` mark
/// could not be written (the earlier spec is complete, so the lane is still startable).
fn run_stop_cmd(args: StopArgs) -> ExitCode {
    let facts = SysinfoFacts::new(claude_config_dir());
    let claude_pid =
        lane_state_writer::claude_parent_pid(std::process::id(), &RealParentProcess::new()).ok();
    let caller = start::caller_role(&state_dir(), claude_pid, &facts);
    let caller_role = caller.as_ref().ok().cloned();
    let requested_by = if args.target_self {
        "self".to_string()
    } else {
        caller_role
            .clone()
            .unwrap_or_else(|| "unverified".to_string())
    };
    let reason_for_log: String = args
        .reason
        .trim()
        .chars()
        .take(start::REASON_MAX_CHARS)
        .collect();
    let ledger = |model: Option<&str>, outcome: &str, acted: bool| {
        log_ledger(
            &requested_by,
            &args.role,
            "stop",
            caller_role.as_deref(),
            model,
            Some(&reason_for_log),
            outcome,
            acted,
        );
    };
    let req = lane_stop::StopRequest {
        role: &args.role,
        reason: &args.reason,
        self_stop: args.target_self,
        caller_claude_pid: claude_pid,
        confirmed: args.confirmed,
        dry_run: args.dry_run,
    };
    let state_dir_path = state_dir();
    let plan = match lane_stop::plan_stop(
        &state_dir_path,
        &req,
        caller,
        &facts,
        &relaunch::policy_default_model(&state_dir_path),
    ) {
        Ok(p) => p,
        Err(refusal) => {
            eprintln!("lane-restart stop: refused - {refusal}");
            ledger(None, &format!("refused: {refusal}"), false);
            return ExitCode::FAILURE;
        }
    };
    let pin_note = plan
        .pinned_from
        .as_deref()
        .map(|was| format!(" model-pinned:{was}"))
        .unwrap_or_default();
    if !plan.will_act {
        println!(
            "lane-restart stop: DRY RUN - would write the launch spec for {} (model {}, HANDOFF {}), \
             then kill pid {} and mark the spec suspended{pin_note}",
            plan.state.role, plan.stopping.model, plan.handoff_path, plan.state.pid
        );
        ledger(
            Some(&plan.stopping.model),
            &format!("dry run - no action taken{pin_note}"),
            false,
        );
        return ExitCode::SUCCESS;
    }
    eprintln!(
        "lane-restart stop: writing the launch spec, then stopping {} (pid {})...",
        plan.state.role, plan.state.pid
    );
    // Captured while claude is still alive: after the kill its parent link is gone.
    let leftover_shell = tab_close::capture(&facts, plan.state.pid, &plan.identity);
    let result = lane_stop::execute_stop(
        &plan,
        &facts,
        &lane_restart::spec::DirSpecStore {
            dir: state_dir_path.clone(),
        },
        &mut std::thread::sleep,
    );
    match result {
        Ok(done) => {
            let warn = done
                .spec_warning
                .as_deref()
                .map(|w| format!("; the launch spec could not be marked suspended: {w}"))
                .unwrap_or_default();
            let exit = if done.spec_warning.is_some() {
                ExitCode::from(4)
            } else {
                ExitCode::SUCCESS
            };
            ledger(
                Some(&plan.stopping.model),
                &format!(
                    "stopped pid {}; launch spec {}{pin_note}{warn}",
                    plan.state.pid,
                    if done.spec_warning.is_some() {
                        "left as stopping (startable)"
                    } else {
                        "suspended"
                    }
                ),
                true,
            );
            // The tab closes LAST: on `--self` this process may share the tab's console.
            close_leftover_tab(
                &facts,
                "stop",
                &requested_by,
                &args.role,
                leftover_shell,
                true,
                plan.state.pid,
                &plan.identity,
            );
            exit
        }
        Err(e) => {
            eprintln!("lane-restart stop: {e}");
            ledger(
                Some(&plan.stopping.model),
                &format!("action failed: {e}{pin_note}"),
                false,
            );
            ExitCode::FAILURE
        }
    }
}

/// `lane-restart specs import [--role R] [--dry-run]` - 14.9. PM only.
fn run_specs_import_cmd(role: Option<String>, dry_run: bool, verbose: bool) -> ExitCode {
    use lane_restart::spec_import::{import_specs, ImportOutcome};
    let facts = SysinfoFacts::new(claude_config_dir());
    let caller = verified_caller(&facts);
    let caller_role = caller.as_ref().ok().cloned();
    let requested_by = caller_role
        .clone()
        .unwrap_or_else(|| "unverified".to_string());
    let target = role.clone().unwrap_or_else(|| "*".to_string());
    if caller_role.as_deref() != Some(start::PM_ROLE) {
        let why = match caller {
            Ok(r) => format!("only the PM session may import specs; this is running under {r:?}"),
            Err(e) => format!(
                "only the PM session may import specs, and the caller could not be verified: {e}"
            ),
        };
        eprintln!("lane-restart specs import: refused - {why}");
        log_ledger(
            &requested_by,
            &target,
            "import",
            caller_role.as_deref(),
            None,
            None,
            &format!("refused: {why}"),
            false,
        );
        return ExitCode::FAILURE;
    }
    let rows = import_specs(
        &state_dir(),
        role.as_deref(),
        &facts,
        &relaunch::policy_default_model(&state_dir()),
        dry_run,
        chrono::Utc::now(),
    );
    let mut refused_named = false;
    for r in &rows {
        match &r.outcome {
            ImportOutcome::Written(s) => {
                log_ledger(
                    &requested_by,
                    &r.role,
                    "import",
                    caller_role.as_deref(),
                    None,
                    None,
                    &format!("imported a launch spec from the lane state file (state={s:?})"),
                    true,
                );
            }
            ImportOutcome::WouldWrite(_) | ImportOutcome::AlreadyHasSpec => {}
            ImportOutcome::Refused(_) => refused_named |= role.is_some(),
        }
    }
    print!(
        "{}",
        lane_restart::spec_import::render(&rows, dry_run, verbose)
    );
    if refused_named {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `lane-restart specs retire --role R... [--dry-run]` - PM only. Moves, never deletes.
fn run_specs_retire_cmd(roles: Vec<String>, dry_run: bool) -> ExitCode {
    use lane_restart::spec_retire::{render, retire_roles, RetireOutcome};
    let facts = SysinfoFacts::new(claude_config_dir());
    let caller = verified_caller(&facts);
    let caller_role = caller.as_ref().ok().cloned();
    let requested_by = caller_role
        .clone()
        .unwrap_or_else(|| "unverified".to_string());
    if caller_role.as_deref() != Some(start::PM_ROLE) {
        let why = match caller {
            Ok(r) => format!("only the PM session may retire roles; this is running under {r:?}"),
            Err(e) => format!(
                "only the PM session may retire roles, and the caller could not be verified: {e}"
            ),
        };
        eprintln!("lane-restart specs retire: refused - {why}");
        log_ledger(
            &requested_by,
            &roles.join(","),
            "retire",
            caller_role.as_deref(),
            None,
            None,
            &format!("refused: {why}"),
            false,
        );
        return ExitCode::FAILURE;
    }
    let rows = retire_roles(&state_dir(), &roles, &facts, dry_run, chrono::Utc::now());
    let mut refused = false;
    for r in &rows {
        match &r.outcome {
            RetireOutcome::Moved(files) => log_ledger(
                &requested_by,
                &r.role,
                "retire",
                caller_role.as_deref(),
                None,
                None,
                &format!("moved {} to retired/", files.join(", ")),
                true,
            ),
            RetireOutcome::WouldMove(_) => {}
            RetireOutcome::Refused(why) => {
                refused = true;
                log_ledger(
                    &requested_by,
                    &r.role,
                    "retire",
                    caller_role.as_deref(),
                    None,
                    None,
                    &format!("refused: {why}"),
                    false,
                );
            }
        }
    }
    print!("{}", render(&rows, dry_run));
    if refused {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `ledger [--role R] [--kind K] [--since T] [--acted|--refused] [--last N] [--json]`.
fn parse_ledger_args(
    rest: &[String],
) -> Result<(lane_restart::ledger::Filter, bool, Option<String>), String> {
    let mut f = lane_restart::ledger::Filter::default();
    let mut json = false;
    let mut since_raw = None;
    let mut iter = rest.iter();
    let value = |name: &str, it: &mut std::slice::Iter<String>| {
        it.next()
            .cloned()
            .ok_or_else(|| format!("{name} requires a value"))
    };
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--role" => f.role = Some(value("--role", &mut iter)?),
            "--kind" => f.kind = Some(value("--kind", &mut iter)?),
            "--since" => since_raw = Some(value("--since", &mut iter)?),
            "--acted" => f.acted = Some(true),
            "--refused" => f.acted = Some(false),
            "--last" => {
                let v = value("--last", &mut iter)?;
                f.last = Some(
                    v.parse()
                        .map_err(|_| format!("--last takes a number: {v:?}"))?,
                );
            }
            "--json" => json = true,
            other => return Err(format!("unrecognised argument: {other}")),
        }
    }
    Ok((f, json, since_raw))
}

/// `lane-restart ledger` - read-only: prints `restart.log` filtered. Writes nothing.
fn run_ledger_cmd(
    mut f: lane_restart::ledger::Filter,
    json: bool,
    since: Option<String>,
) -> ExitCode {
    use lane_restart::ledger;
    if let Some(s) = since {
        match ledger::parse_since(&s, chrono::Utc::now()) {
            Ok(t) => f.since = Some(t),
            Err(e) => {
                eprintln!("lane-restart ledger: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    let read = match ledger::read(&log_path()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("lane-restart ledger: {e}");
            return ExitCode::FAILURE;
        }
    };
    let shown = ledger::select(&read.rows, &f);
    if json {
        for r in &shown {
            match serde_json::to_string(r) {
                Ok(l) => println!("{l}"),
                Err(e) => {
                    eprintln!("lane-restart ledger: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
        eprintln!(
            "({} of {} entries shown; {} unreadable line(s))",
            shown.len(),
            read.rows.len(),
            read.unreadable.len()
        );
    } else {
        print!(
            "{}",
            ledger::render(&shown, read.rows.len(), &read.unreadable)
        );
    }
    ExitCode::SUCCESS
}

fn parse_assert_idle_args(rest: &[String]) -> Result<Option<String>, ArgError> {
    let mut role = None;
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--role" => role = Some(iter.next().ok_or(ArgError::MissingRole)?.clone()),
            other => return Err(ArgError::Unknown(other.to_string())),
        }
    }
    Ok(role)
}

const USAGE: &str = "\
lane-restart - a bulletproof session-restart tool. RESTART-TOOL-DESIGN.md.

USAGE:
    lane-restart --role <name> [--self] [--dry-run] [--yes]
        Restart a lane. --self restarts the CALLING lane (no idle check);
        omitting it targets a DIFFERENT lane, which must be independently
        idle and requires --yes to act for real - otherwise it prints a dry
        run and does nothing.
        Exit codes (PM finding, 2026-09-19: distinct codes so scripts can
        tell these apart): 0 = relaunched with real progress confirmed;
        2 = killed and relaunched, but awaiting a human at the dev-channels
        confirmation dialog; 1 = refused, or the relaunch failed outright.

    lane-restart state <event>
        The hook subcommand: reads a hook's JSON input on stdin and updates
        .lane-state/<role>.json. Wired into settings.json; never run this by
        hand.

    lane-restart assert-idle [--role <name>]
        Run by a lane ITSELF, from its own shell, right after writing
        HANDOFF: asserts that no background shell it started is still
        running. Also WARNS (exit code unchanged) when the HANDOFF is over
        its size ceiling: 8 KB for a lane, 20 KB for the PM
        (CIRESNAVE-EXPECTATIONS.md 2.3c). Required before any restart of that lane can be
        authorized - RESTART-TOOL-DESIGN.md §1a. Role defaults to
        LANE_ROLE, then falls back to the current directory's leaf name,
        the same as the state hook.

    lane-restart host --role <name> -- <argv...>
        Internal: runs inside the relaunched session's own terminal tab
        (RESTART-TOOL-DESIGN.md §12.5). Loads the user's approvals, owns a
        ConPTY, hosts <argv...> (normally `claude ...`) inside it, relays
        every byte transparently, and - only during the startup window -
        checks the screen against every approval (RESTART-TOOL-DESIGN.md
        §12), injecting the matching one's action. If the window ends on a
        dialog nothing answered, it messages the notify role's lane.
        Wired automatically by a restart; never run this by hand.

    lane-restart approvals [--role <name>]
        Fetch the approvals configured in ~/.overmind/lane-restart.json
        (from that repo's default branch only) and list every active and
        refused one, exactly as `host` would load them.

    lane-restart status [--json]
        Read-only: each role's state (running, suspended, crashed, never_run,
        unknown), whether its launch spec (<role>.launch.json, written before a
        restart's kill and at a confirmed launch) is complete, and whether it can
        be started again from it. RESTART-TOOL-DESIGN.md section 14.

    lane-restart start --role <name> --reason <text> [--model sonnet|haiku] [--dry-run]
        Start a lane that is not running, from its launch spec
        (<role>.launch.json), and write the spec back as running once the
        session is confirmed. PM ONLY: the caller's claude session must be
        the pid and start time recorded by pm's state file (a lane cannot
        claim it with LANE_ROLE; a lane that edits pm.json can forge it -
        this closes the accidental and prompt-injected paths, not a hostile
        local user). --reason is required (<= 200 chars, no control
        characters) and goes to restart.log, not into the prompt. The model
        is sonnet or haiku (default: the spec's, else sonnet); Opus is
        refused and LANE_RESTART_ALLOW_OPUS is not read. The permission mode
        is always auto. Starting role pm is refused: it is started by a
        person. Exit codes as a restart: 0, 2 (awaiting the dialog), 1.

    lane-restart stop --role <name> --reason <text> [--self] [--dry-run] [--yes]
        Suspend a lane. The complete launch spec (<role>.launch.json) is
        written durably and read back BEFORE the process is touched, then the
        verified kill, then the spec is marked suspended: a lane is stopped
        only if it can be started again. Needs a HANDOFF written during the
        lane's session (and not after it asserted idle); there is no
        override. Another lane: PM only (same process-ancestry check as
        start), idle, and --yes to act for real, otherwise a dry run. --self
        stops the calling lane. Role pm is refused. --reason as for start.
        Exit codes: 0 stopped and suspended; 1 refused or failed; 4 stopped,
        but the suspended mark could not be written (still startable).

    lane-restart specs import [--role <name>] [--dry-run] [--verbose]
        PM only. The first and last lines say whether anything was written
        (DRY RUN - nothing written / WROTE n spec(s)). Refused roles are all
        listed; roles refused for the same reason (e.g. cwd missing) share one
        line with a count unless --verbose. For every lane state file with no launch spec, build
        <role>.launch.json from its recorded launch_args, labelled
        written_by=import, only if the spec is complete; the rest are listed
        with the reason. Never overwrites a spec; never edits a state file.

    lane-restart specs retire --role <name> [--role <name>]... [--dry-run]
        PM only. For each role whose recorded cwd is not a directory and whose
        recorded process is not live, MOVE its state file (and launch spec, if
        any) to <state dir>/retired/<file>.<UTC stamp>; never deletes, never
        overwrites, reads the move back. Role pm, a role with a directory
        (resume it with `start`) and a role with a live process are refused
        with the reason. First and last lines say whether anything moved.

    lane-restart ledger [--role <name>] [--kind <kind>] [--since <time|30m|2h|1d>]
                        [--acted | --refused] [--last <n>] [--json]
        Read-only: prints restart.log, oldest first: time, role, kind
        (restart if the line predates kinds), who asked (and the verified
        caller if it differs), acted or refused, and the outcome. A line that
        is not an entry is counted and named in the footer, never skipped
        silently. Writes nothing.

    lane-restart models
        Read-only: list each lane's recorded model and flag Opus. A relaunch
        pins `--model` from the original launch's explicit --model, else
        `model-policy.json` (default sonnet) - never the recorded model - and
        refuses Opus unless LANE_RESTART_ALLOW_OPUS=1.

    lane-restart --version
        Print the crate version and the configured approvals source. This
        binary embeds no approvals (RESTART-TOOL-DESIGN.md §12.2).

    lane-restart --help
        Print this message.
";

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if argv.iter().any(|a| a == "--version" || a == "-V") {
        print_version();
        return ExitCode::SUCCESS;
    }
    if let [cmd, event] = argv.as_slice() {
        if cmd == "state" {
            return run_state_hook(event);
        }
    }
    if let Some((first, rest)) = argv.split_first() {
        if first == "assert-idle" {
            return match parse_assert_idle_args(rest) {
                Ok(role_override) => run_assert_idle_cmd(role_override),
                Err(e) => {
                    eprintln!("lane-restart assert-idle: {e}");
                    ExitCode::FAILURE
                }
            };
        }
        if first == "status" {
            return match rest {
                [] => run_status_cmd(false),
                [flag] if flag == "--json" => run_status_cmd(true),
                _ => {
                    eprintln!("lane-restart status: expected `status [--json]`");
                    ExitCode::FAILURE
                }
            };
        }
        if first == "start" {
            return match parse_start_args(rest) {
                Ok(a) => run_start_cmd(a),
                Err(e) => {
                    eprintln!("lane-restart start: {e}");
                    ExitCode::FAILURE
                }
            };
        }
        if first == "stop" {
            return match parse_stop_args(rest) {
                Ok(a) => run_stop_cmd(a),
                Err(e) => {
                    eprintln!("lane-restart stop: {e}");
                    ExitCode::FAILURE
                }
            };
        }
        if first == "specs" {
            return match rest.split_first() {
                Some((sub, tail)) if sub == "import" => match parse_specs_import_args(tail) {
                    Ok((role, dry_run, verbose)) => run_specs_import_cmd(role, dry_run, verbose),
                    Err(e) => {
                        eprintln!("lane-restart specs import: {e}");
                        ExitCode::FAILURE
                    }
                },
                Some((sub, tail)) if sub == "retire" => match parse_specs_retire_args(tail) {
                    Ok((roles, dry_run)) => run_specs_retire_cmd(roles, dry_run),
                    Err(e) => {
                        eprintln!("lane-restart specs retire: {e}");
                        ExitCode::FAILURE
                    }
                },
                _ => {
                    eprintln!(
                        "lane-restart specs: expected `specs import [--role <name>] [--dry-run] [--verbose]` or `specs retire --role <name>... [--dry-run]`"
                    );
                    ExitCode::FAILURE
                }
            };
        }
        if first == "ledger" {
            return match parse_ledger_args(rest) {
                Ok((f, json, since)) => run_ledger_cmd(f, json, since),
                Err(e) => {
                    eprintln!("lane-restart ledger: {e}");
                    ExitCode::FAILURE
                }
            };
        }
        if first == "models" && rest.is_empty() {
            print!("{}", relaunch::models_report(&state_dir()));
            return ExitCode::SUCCESS;
        }
        if first == "approvals" {
            return match parse_assert_idle_args(rest) {
                Ok(role) => run_approvals_cmd(role),
                Err(e) => {
                    eprintln!("lane-restart approvals: {e}");
                    ExitCode::FAILURE
                }
            };
        }
        if first == "host" {
            return match parse_host_args(rest) {
                Ok((role, child_argv)) => run_host_cmd(&role, &child_argv),
                Err(e) => {
                    eprintln!("lane-restart host: {e}");
                    ExitCode::FAILURE
                }
            };
        }
    }
    let args = match parse_args(argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("lane-restart: {e}");
            return ExitCode::FAILURE;
        }
    };

    let target = if args.target_self {
        Target::Myself {
            role: args.role.clone(),
            // The same ancestry walk the hooks use to record `pid`: this
            // process <- shell(s) <- claude. `decide` compares it with the
            // state file's pid, so `--self` is a claim it can check.
            caller_pid: lane_state_writer::claude_parent_pid(
                std::process::id(),
                &RealParentProcess::new(),
            )
            .ok(),
        }
    } else {
        Target::Other {
            role: args.role.clone(),
        }
    };

    let facts = SysinfoFacts::new(claude_config_dir());
    // 14.7 / 14.8: `requested_by` is the VERIFIED caller (the role whose claude session ran
    // this), not a label chosen by the absence of `--self`. A caller that cannot be verified is
    // recorded as "unverified"; a restart of another lane is still not refused on it (the
    // `--yes` + idle checks are unchanged), so the ledger is where a non-PM caller shows up.
    let caller = (!args.target_self).then(|| verified_caller(&facts));
    let caller_role = caller.as_ref().and_then(|c| c.as_ref().ok().cloned());
    let requested_by: &str = if args.target_self {
        "self"
    } else {
        caller_role.as_deref().unwrap_or("unverified")
    };
    // RESTART-TOOL-DESIGN.md 14.7 / 14.8, the PM's ruling of 2026-10-11: restarting ANOTHER lane
    // is the PM session's alone, the same rule as `stop`. Refused (and logged) before anything
    // about the target is read, even for a dry run.
    if let Some(caller) = caller {
        if let Err(refusal) = lane_stop::require_pm_for_other(false, caller) {
            eprintln!("lane-restart: refused - {refusal}");
            log_outcome(
                requested_by,
                &args.role,
                &format!("refused: {refusal}"),
                false,
            );
            return ExitCode::FAILURE;
        }
    }
    let req = authorize::Request {
        target,
        confirmed: args.confirmed,
        dry_run: args.dry_run,
    };

    match authorize::decide(&req, &facts, &state_dir()) {
        Err(refusal) => {
            eprintln!("lane-restart: refused - {refusal}");
            log_outcome(
                requested_by,
                &args.role,
                &format!("refused: {refusal}"),
                false,
            );
            ExitCode::FAILURE
        }
        Ok(plan) if !plan.will_act => {
            match relaunch::describe_dry_run(&plan.state) {
                Ok(argv) => {
                    let cmdline: Vec<String> = argv
                        .iter()
                        .map(|a| {
                            if a.contains(' ') {
                                format!("{a:?}")
                            } else {
                                a.clone()
                            }
                        })
                        .collect();
                    println!(
                        "lane-restart: DRY RUN - would kill pid {} and relaunch a FRESH session \
                         via wt.exe (conhost.exe fallback), hosted by \
                         `lane-restart host --role {} -- {}`, in {}",
                        plan.state.pid,
                        plan.state.role,
                        cmdline.join(" "),
                        plan.state.cwd
                    );
                }
                Err(e) => {
                    println!(
                        "lane-restart: DRY RUN - would kill pid {} but the relaunch command \
                         itself would be refused: {e}",
                        plan.state.pid
                    );
                }
            }
            match tab_close::capture(&facts, plan.state.pid, &plan.identity) {
                Ok(shell) => println!(
                    "lane-restart: DRY RUN - would then close the leftover tab by terminating \
                     {shell}, if it has no other children by then"
                ),
                Err(why) => {
                    println!("lane-restart: DRY RUN - would leave the tab's shell alone: {why}")
                }
            }
            log_outcome(requested_by, &args.role, "dry run - no action taken", false);
            ExitCode::SUCCESS
        }
        Ok(plan) => {
            // Before the kill: after it, a refusal would leave a lane dead and not relaunched.
            if let Err(refusal) = disk_gate("restart") {
                log_outcome(
                    requested_by,
                    &args.role,
                    &format!("refused: {refusal}"),
                    false,
                );
                return ExitCode::FAILURE;
            }
            eprintln!(
                "lane-restart: killing pid {} and relaunching a fresh session...",
                plan.state.pid
            );
            let state_dir_path = state_dir();
            let state_reader = relaunch::RealStateReader {
                state_dir: &state_dir_path,
            };
            let role_for_notice = plan.state.role.clone();
            let mut on_awaiting = || {
                // PM finding, 2026-09-18 (fifth real-restart retest):
                // printed immediately, the moment this is first detected -
                // never only at the end of the (up to ~15 minute) wait -
                // so whatever reads this stream (the PM lane, today) can
                // notify CireSnave promptly that the dev-channels dialog
                // needs a human.
                print_status_json("awaiting_confirmation", &role_for_notice);
            };
            // Captured while claude is still alive, moments before `kill_and_relaunch` kills it:
            // after the kill, claude's parent link is gone with it.
            let leftover_shell = tab_close::capture(&facts, plan.state.pid, &plan.identity);
            let relaunch_result = relaunch::kill_and_relaunch(
                &facts,
                &state_reader,
                &plan.state,
                &plan.identity,
                &mut on_awaiting,
            );
            // The tab closes LAST: on `--self`, this process may share the tab's console and go
            // with it, so everything else is already logged by then.
            let relaunch_acted = relaunch_result.is_ok();
            let exit = match relaunch_result {
                Ok(outcome @ relaunch::RelaunchOutcome::Relaunched) => {
                    log_outcome(
                        requested_by,
                        &args.role,
                        &format!(
                            "killed pid {} and relaunched a fresh session (continuity via HANDOFF only)",
                            plan.state.pid
                        ),
                        true,
                    );
                    relaunch_exit_code(outcome)
                }
                Ok(outcome @ relaunch::RelaunchOutcome::AwaitingConfirmation) => {
                    log_outcome(
                        requested_by,
                        &args.role,
                        "awaiting human confirmation at the dev-channels dialog",
                        true,
                    );
                    relaunch_exit_code(outcome)
                }
                Err(e) => {
                    eprintln!("lane-restart: {e}");
                    log_outcome(
                        requested_by,
                        &args.role,
                        &format!("action failed: {e}"),
                        false,
                    );
                    ExitCode::FAILURE
                }
            };
            close_leftover_tab(
                &facts,
                "restart",
                requested_by,
                &args.role,
                leftover_shell,
                relaunch_acted,
                plan.state.pid,
                &plan.identity,
            );
            exit
        }
    }
}

/// RESTART-TOOL-DESIGN.md §5: closes the tab a hand-started lane's shell keeps open once its
/// claude is killed (`tab_close.rs`). Only after a relaunch that acted - after a failed one the
/// tab stays, so a human has a prompt to relaunch from. Every outcome is logged, and a closed
/// shell is logged BEFORE the kill, in case closing the tab takes this process with it.
#[allow(clippy::too_many_arguments)]
fn close_leftover_tab(
    facts: &SysinfoFacts,
    kind: &str,
    requested_by: &str,
    role: &str,
    leftover_shell: Result<tab_close::ShellCandidate, tab_close::LeftAlone>,
    relaunch_acted: bool,
    killed_pid: u32,
    killed_identity: &lane_restart::facts::ProcessIdentity,
) {
    let result = leftover_shell.and_then(|shell| {
        if !relaunch_acted {
            return Err(tab_close::LeftAlone::RelaunchFailed);
        }
        tab_close::close(facts, &shell, killed_pid, killed_identity, &mut |shell| {
            log_kind(
                kind,
                requested_by,
                role,
                &format!("closing the leftover tab: terminating shell {shell}"),
                true,
            );
        })
    });
    if let Err(why) = result {
        log_kind(
            kind,
            requested_by,
            role,
            &format!("leftover tab's shell left alone: {why}"),
            false,
        );
    }
}

/// PM finding, 2026-09-19 (real-restart-2): exit 0 is indistinguishable
/// from `Relaunched` for anything scripting against this tool - a
/// genuinely different, real outcome needs a genuinely different exit
/// code. `AwaitingConfirmation` is real, ACTED work (§5) but not the same
/// as confirmed progress, so it gets its own code rather than sharing
/// `Relaunched`'s 0 or `Err`'s 1.
fn relaunch_exit_code(outcome: relaunch::RelaunchOutcome) -> ExitCode {
    match outcome {
        relaunch::RelaunchOutcome::Relaunched => ExitCode::SUCCESS,
        relaunch::RelaunchOutcome::AwaitingConfirmation => ExitCode::from(2),
    }
}

/// A machine-readable status line - PM finding, 2026-09-18 (fifth real-
/// restart retest): so the PM lane (or anything else watching this
/// process's stdout) can act on `awaiting_confirmation` promptly, e.g. by
/// sending CireSnave a phone notification that a relaunched lane needs a
/// human to confirm the dev-channels dialog.
fn print_status_json(outcome: &str, role: &str) {
    let line = serde_json::json!({
        "event": "lane-restart-status",
        "role": role,
        "outcome": outcome,
        "at": chrono::Utc::now().to_rfc3339(),
    });
    println!("{line}");
}

/// A restart-path ledger line (`kind: restart`).
fn log_outcome(requested_by: &str, role: &str, outcome: &str, acted: bool) {
    log_kind("restart", requested_by, role, outcome, acted);
}

/// A ledger line of `kind`, with `caller_role` set whenever `requested_by` is a verified role
/// (that is, neither the `--self` label nor "unverified").
fn log_kind(kind: &str, requested_by: &str, role: &str, outcome: &str, acted: bool) {
    let caller_role =
        (requested_by != "self" && requested_by != "unverified").then_some(requested_by);
    log_ledger(
        requested_by,
        role,
        kind,
        caller_role,
        None,
        None,
        outcome,
        acted,
    );
}

/// One `restart.log` line with the 14.7 fields.
#[allow(clippy::too_many_arguments)]
fn log_ledger(
    requested_by: &str,
    role: &str,
    kind: &str,
    caller_role: Option<&str>,
    model: Option<&str>,
    reason: Option<&str>,
    outcome: &str,
    acted: bool,
) {
    let entry = log::Entry {
        timestamp: chrono::Utc::now(),
        requested_by,
        role,
        outcome: outcome.to_string(),
        acted,
        kind: Some(kind),
        caller_role,
        model,
        reason,
    };
    if let Err(e) = log::append(&log_path(), &entry) {
        eprintln!("lane-restart: WARNING - could not write to the restart log: {e}");
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    fn strs(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn assert_idle_with_no_args_has_no_role_override() {
        assert_eq!(parse_assert_idle_args(&[]), Ok(None));
    }

    #[test]
    fn assert_idle_role_flag_is_extracted() {
        assert_eq!(
            parse_assert_idle_args(&strs(&["--role", "pm"])),
            Ok(Some("pm".to_string()))
        );
    }

    #[test]
    fn assert_idle_role_flag_with_no_value_is_an_error() {
        assert_eq!(
            parse_assert_idle_args(&strs(&["--role"])),
            Err(ArgError::MissingRole)
        );
    }

    #[test]
    fn assert_idle_rejects_an_unknown_flag() {
        assert_eq!(
            parse_assert_idle_args(&strs(&["--bogus"])),
            Err(ArgError::Unknown("--bogus".to_string()))
        );
    }

    /// ⚠️ PM finding, 2026-09-18: `--help` printed "unrecognised argument:
    /// --help" because `parse_args` treats every unknown token as an error
    /// with no earlier check. `main()` now intercepts `--help`/`-h` before
    /// any subcommand dispatch - this asserts the usage text actually
    /// documents the command this fix itself adds, so the two can't drift
    /// apart silently.
    #[test]
    fn usage_text_documents_assert_idle() {
        assert!(USAGE.contains("assert-idle"));
        assert!(USAGE.contains("--help"));
    }

    #[test]
    fn usage_text_documents_host_and_version() {
        assert!(USAGE.contains(" host "));
        assert!(USAGE.contains("--version"));
        assert!(USAGE.contains("lane-restart approvals"));
    }

    // -- relaunch_exit_code -------------------------------------------- //
    // PM finding, 2026-09-19 (real-restart-2): exit 0 was indistinguishable
    // from a genuine `Relaunched` for anything scripting against this tool.

    #[test]
    fn relaunched_exits_zero() {
        assert_eq!(
            relaunch_exit_code(relaunch::RelaunchOutcome::Relaunched),
            ExitCode::SUCCESS
        );
    }

    #[test]
    fn awaiting_confirmation_exits_two_not_zero() {
        let code = relaunch_exit_code(relaunch::RelaunchOutcome::AwaitingConfirmation);
        assert_ne!(
            code,
            ExitCode::SUCCESS,
            "AwaitingConfirmation must be distinguishable from Relaunched"
        );
        assert_eq!(code, ExitCode::from(2));
    }

    // -- start / specs import ------------------------------------------ //

    #[test]
    fn start_args_carry_role_reason_model_and_dry_run() {
        assert_eq!(
            parse_start_args(&strs(&[
                "--role",
                "baracuda",
                "--reason",
                "resume after crash",
                "--model",
                "haiku",
                "--dry-run"
            ])),
            Ok(StartArgs {
                role: "baracuda".into(),
                reason: "resume after crash".into(),
                model: Some("haiku".into()),
                dry_run: true,
            })
        );
    }

    /// The reason is validated by `start::validate_reason`, once, so a missing one reaches it
    /// as an empty string rather than failing here with a second message.
    #[test]
    fn start_without_a_reason_parses_to_an_empty_reason() {
        let a = parse_start_args(&strs(&["--role", "baracuda"])).unwrap();
        assert_eq!(a.reason, "");
        assert!(start::validate_reason(&a.reason).is_err());
    }

    #[test]
    fn start_requires_a_role_and_values_for_its_flags() {
        assert_eq!(
            parse_start_args(&strs(&["--reason", "x"])),
            Err(ArgError::RoleRequired)
        );
        assert_eq!(
            parse_start_args(&strs(&["--role", "a", "--reason"])),
            Err(ArgError::MissingValue("--reason"))
        );
        assert_eq!(
            parse_start_args(&strs(&["--role", "a", "--model"])),
            Err(ArgError::MissingValue("--model"))
        );
        assert_eq!(
            parse_start_args(&strs(&["--role", "a", "--bogus"])),
            Err(ArgError::Unknown("--bogus".into()))
        );
    }

    #[test]
    fn stop_args_are_role_reason_self_dry_run_and_yes() {
        assert_eq!(
            parse_stop_args(&strs(&[
                "--role",
                "baracuda",
                "--reason",
                "end of shift",
                "--self",
                "--dry-run",
                "--yes",
            ])),
            Ok(StopArgs {
                role: "baracuda".into(),
                reason: "end of shift".into(),
                target_self: true,
                dry_run: true,
                confirmed: true,
            })
        );
        let a = parse_stop_args(&strs(&["--role", "baracuda"])).unwrap();
        assert_eq!(a.reason, "", "a missing reason reaches validate_reason");
        assert!(!a.target_self && !a.dry_run && !a.confirmed);
        assert_eq!(
            parse_stop_args(&strs(&["--reason", "x"])),
            Err(ArgError::RoleRequired)
        );
        assert_eq!(
            parse_stop_args(&strs(&["--role", "a", "--reason"])),
            Err(ArgError::MissingValue("--reason"))
        );
        assert_eq!(
            parse_stop_args(&strs(&["--role", "a", "--model", "sonnet"])),
            Err(ArgError::Unknown("--model".into()))
        );
    }

    #[test]
    fn specs_import_args_are_role_and_dry_run() {
        assert_eq!(parse_specs_import_args(&[]), Ok((None, false, false)));
        assert_eq!(
            parse_specs_import_args(&strs(&["--role", "baracuda", "--dry-run", "--verbose"])),
            Ok((Some("baracuda".into()), true, true))
        );
        assert_eq!(
            parse_specs_import_args(&strs(&["--yes"])),
            Err(ArgError::Unknown("--yes".into()))
        );
    }

    #[test]
    fn specs_retire_args_need_a_role_and_repeat() {
        assert_eq!(parse_specs_retire_args(&[]), Err(ArgError::RoleRequired));
        assert_eq!(
            parse_specs_retire_args(&strs(&["--role", "a", "--role", "b", "--dry-run"])),
            Ok((vec!["a".to_string(), "b".to_string()], true))
        );
        assert_eq!(
            parse_specs_retire_args(&strs(&["--all"])),
            Err(ArgError::Unknown("--all".into()))
        );
        assert!(USAGE.contains("lane-restart specs retire"));
    }

    #[test]
    fn ledger_args_parse_filters_and_refuse_the_unknown() {
        let (f, json, since) = parse_ledger_args(&strs(&[
            "--role",
            "a",
            "--kind",
            "start",
            "--refused",
            "--last",
            "5",
            "--since",
            "2h",
            "--json",
        ]))
        .unwrap();
        assert_eq!(f.role.as_deref(), Some("a"));
        assert_eq!(f.kind.as_deref(), Some("start"));
        assert_eq!((f.acted, f.last, json), (Some(false), Some(5), true));
        assert_eq!(since.as_deref(), Some("2h"));
        assert!(parse_ledger_args(&strs(&["--last", "x"])).is_err());
        assert!(parse_ledger_args(&strs(&["--role"])).is_err());
        assert!(parse_ledger_args(&strs(&["--all"])).is_err());
        assert!(USAGE.contains("lane-restart ledger"));
    }

    #[test]
    fn usage_text_documents_start_and_specs_import() {
        assert!(USAGE.contains("lane-restart stop --role"));
        assert!(USAGE.contains("lane-restart start --role"));
        assert!(USAGE.contains("lane-restart specs import"));
    }

    // -- parse_host_args ---------------------------------------------- //

    #[test]
    fn host_args_extracts_role_and_child_argv() {
        assert_eq!(
            parse_host_args(&strs(&[
                "--role", "overmind", "--", "claude", "--name", "x"
            ])),
            Ok(("overmind".to_string(), strs(&["claude", "--name", "x"])))
        );
    }

    #[test]
    fn host_args_requires_a_separator() {
        assert_eq!(
            parse_host_args(&strs(&["--role", "overmind"])),
            Err(ArgError::MissingChildArgv)
        );
    }

    #[test]
    fn host_args_requires_a_non_empty_child_argv_after_the_separator() {
        assert_eq!(
            parse_host_args(&strs(&["--role", "overmind", "--"])),
            Err(ArgError::MissingChildArgv)
        );
    }

    #[test]
    fn host_args_requires_role() {
        assert_eq!(
            parse_host_args(&strs(&["--", "claude"])),
            Err(ArgError::RoleRequired)
        );
    }

    #[test]
    fn host_args_rejects_an_unknown_flag_before_the_separator() {
        assert_eq!(
            parse_host_args(&strs(&["--bogus"])),
            Err(ArgError::Unknown("--bogus".to_string()))
        );
    }
}
