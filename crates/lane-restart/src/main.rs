// SPDX-License-Identifier: MIT OR Apache-2.0

//! CLI entry point. All the actual decision logic lives in the library
//! (`authorize.rs`, `facts.rs`, `state.rs`, `log.rs`) so it can be unit
//! tested without a real process. This file only: parses argv, wires the
//! real `SystemFacts`, calls `authorize::decide`, and - only if it comes
//! back `Ok` with `will_act: true` - performs the kill and the relaunch.

use lane_restart::authorize::{self, Target};
use lane_restart::facts::SysinfoFacts;
use lane_restart::lane_state_writer::{self, ParentProcess, RealParentProcess};
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
    /// `None` only with `--self` and no `--role`: resolved in `main` before use.
    role: Option<String>,
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
    if role.is_none() && !target_self {
        return Err(ArgError::RoleRequired);
    }
    Ok(Args {
        role,
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

/// `lane-restart attach --role <r> --watch` (`docs/PORT-LINUX-PLAN.md`, P6b): the screen of a running
/// lane host and its live output, read-only. `--watch` is REQUIRED: interactive attach is not built
/// and not approved (P6c), and a bare `attach` must not look like it will do that. Read-only, so it
/// is open to any session, like `status`.
fn run_attach_cmd(rest: &[String]) -> ExitCode {
    let mut role: Option<String> = None;
    let mut watch = false;
    let mut iter = rest.iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--role" => role = iter.next().cloned(),
            "--watch" => watch = true,
            other => {
                eprintln!("lane-restart attach: unexpected argument {other:?}");
                return ExitCode::FAILURE;
            }
        }
    }
    let Some(role) = role.filter(|r| lane_restart::relaunch::valid_identifier(r)) else {
        eprintln!("lane-restart attach: --role <name> is required and must be a plain identifier");
        return ExitCode::FAILURE;
    };
    if !watch {
        eprintln!(concat!(
            "lane-restart attach: --watch is required. Only read-only watching exists; ",
            "interactive attach is not built (it is not approved until a threat-model note ",
            "says how a same-user agent is kept off the socket)."
        ));
        return ExitCode::FAILURE;
    }
    #[cfg(unix)]
    {
        let path = lane_restart::watch::socket_path(&state_dir(), &role);
        eprintln!(
            "lane-restart attach: watching {role} (read-only; the host closes this when it ends)"
        );
        match lane_restart::watch::connect_and_copy(&path, &mut std::io::stdout()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!(
                    "lane-restart attach: {}: {e} (is a direct host running for {role}?)",
                    path.display()
                );
                ExitCode::FAILURE
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = role;
        eprintln!(
            "lane-restart attach: not available on this OS (a direct lane host, and so its watch              socket, exists only off Windows)"
        );
        ExitCode::FAILURE
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

/// ` QUIET>Ns` after the quiet seconds when `--quiet-over N` was given and the role is over it.
fn quiet_mark(a: &lane_restart::spec::Activity, quiet_over: Option<i64>) -> String {
    match (a.quiet_over, quiet_over) {
        (Some(true), Some(n)) => format!(" QUIET>{n}s"),
        _ => String::new(),
    }
}

/// `status [--json] [--quiet-over <secs>]` arguments, any order. The threshold is a positive
/// whole number of seconds; no default is applied (the flag is opt-in).
fn parse_status_args(args: &[String]) -> Result<(bool, Option<i64>, bool), String> {
    let (mut json, mut quiet, mut peers) = (false, None, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => json = true,
            "--peers" => peers = true,
            "--quiet-over" => {
                let v = it.next().ok_or("--quiet-over needs a number of seconds")?;
                let n: i64 =
                    v.parse().ok().filter(|n| *n > 0).ok_or_else(|| {
                        format!("--quiet-over: not a positive whole number: {v:?}")
                    })?;
                if quiet.replace(n).is_some() {
                    return Err("--quiet-over given twice".to_string());
                }
            }
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    Ok((json, quiet, peers))
}

/// `\tduplicate-risk: host pid N, claude pid M`, or nothing for a normal row.
fn duplicate_text(d: &[lane_restart::duplicate::Duplicate]) -> String {
    if d.is_empty() {
        return String::new();
    }
    let list: Vec<String> = d.iter().map(ToString::to_string).collect();
    format!("\tduplicate-risk: {}", list.join(", "))
}

fn peers_text(p: &lane_restart::peers_join::RolePeers) -> String {
    use lane_restart::peers_join::RolePeers;
    match p {
        RolePeers::Unknown(u) => u.clone(),
        RolePeers::Peers(v) if v.is_empty() => "-".to_string(),
        RolePeers::Peers(v) => v
            .iter()
            .map(|p| match &p.summary {
                Some(s) => format!("{} {s:?}", p.id),
                None => p.id.clone(),
            })
            .collect::<Vec<_>>()
            .join("; "),
    }
}

/// `lane-restart status [--json]` - RESTART-TOOL-DESIGN.md 14.2 / 14.10. Read-only: for every
/// role with a lane state or a launch spec, whether it is running, suspended, crashed or never
/// run, and whether it can be started again from its spec. Liveness is the process table's
/// (pid and start time), never a file's age.
fn run_status_cmd(json: bool, quiet_over: Option<i64>, with_peers: bool) -> ExitCode {
    use lane_restart::spec;
    let facts = SysinfoFacts::new(claude_config_dir());
    let mut report = spec::status_all(&state_dir(), &facts);
    if let Some(secs) = quiet_over {
        report.mark_quiet_over(secs);
    }
    lane_restart::duplicate::annotate(&mut report, &state_dir(), &facts);
    if with_peers {
        let parents = RealParentProcess::new();
        lane_restart::peers_join::annotate(
            &mut report,
            &state_dir(),
            &facts,
            &|pid| {
                lane_restart::lane_state_writer::ParentProcess::parent_of(&parents, pid)
                    .map(|p| p.0)
            },
            lane_restart::notify::list_all(lane_restart::notify::broker_addr()),
        );
    }
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
            "{}\t{:?}\tspec={:?}\tstartable={}\t{}{}{}",
            r.role,
            r.state,
            r.spec,
            r.startable,
            match &r.activity {
                Some(a) if a.idle => format!(
                    "idle=true quiet={}s{}",
                    a.quiet_secs,
                    quiet_mark(a, quiet_over)
                ),
                Some(a) => format!(
                    "idle=false({}) quiet={}s{}",
                    a.idle_blockers.join("+"),
                    a.quiet_secs,
                    quiet_mark(a, quiet_over)
                ),
                None => "idle=-".to_string(),
            },
            match &r.handoff {
                Some(h) => format!(
                    "\thandoff={}B/{}B{}",
                    h.bytes,
                    h.ceiling,
                    if h.over { " OVER" } else { "" }
                ),
                None => "\thandoff=none".to_string(),
            } + &r
                .nested_in
                .as_deref()
                .map(|p| format!("\tnested-in={p}"))
                .unwrap_or_default(),
            r.detail
                .as_deref()
                .map(|d| format!("\t{d}"))
                .unwrap_or_default()
                + &duplicate_text(&r.duplicate_risk)
        );
        if let Some(p) = &r.peers {
            println!("  peers: {}", peers_text(p));
        }
    }
    if let Some(e) = &report.duplicate_error {
        println!(
            "(duplicate-risk: the process table could not be read, so no role was checked: {e})"
        );
    }
    if let Some(e) = &report.peers_error {
        println!("(peers: the broker could not be read, so every role is unknown: {e})");
    }
    if let Some(un) = &report.peers_unmatched {
        for u in un {
            println!(
                "unmatched peer {} in {}: no role's live claude is its parent{}",
                u.id,
                u.cwd,
                u.summary
                    .as_deref()
                    .map(|s| format!(" - {s:?}"))
                    .unwrap_or_default()
            );
        }
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
    json: bool,
    signal_id: Option<String>,
}

/// `start --role R --reason <text> [--model sonnet|haiku] [--dry-run]`. A missing `--reason` is
/// not an argument error: it reaches `start::validate_reason`, so there is one message for it.
fn parse_start_args(rest: &[String]) -> Result<StartArgs, ArgError> {
    let mut role = None;
    let mut reason = String::new();
    let mut model = None;
    let mut dry_run = false;
    let mut json = false;
    let mut signal_id = None;
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--signal-id" => {
                signal_id = Some(
                    iter.next()
                        .ok_or(ArgError::MissingValue("--signal-id"))?
                        .clone(),
                )
            }
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
        json,
        signal_id,
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

/// The caller of a `start`: the PM session by process (as for every command), else the launcher
/// service by its record and by being this process's direct parent. Only `start` asks for the
/// second; `stop` and a restart read `verified_caller` alone and so never see `overmind`.
fn verified_start_caller(facts: &SysinfoFacts) -> Result<String, start::CallerError> {
    match verified_caller(facts) {
        Ok(role) => Ok(role),
        Err(first) => {
            let parent = RealParentProcess::new()
                .parent_of(std::process::id())
                .map(|(pid, _)| pid);
            start::launcher_caller(&state_dir(), parent, facts).map_err(|_| first)
        }
    }
}

/// `lane-restart start --role R --reason <text> [--model sonnet|haiku] [--signal-id <id>]
/// [--dry-run] [--json]` - 14.5.
fn run_start_cmd(args: StartArgs) -> ExitCode {
    let facts = SysinfoFacts::new(claude_config_dir());
    let caller = verified_start_caller(&facts);
    let caller_role = caller.as_ref().ok().cloned();
    let requested_by = caller_role
        .clone()
        .unwrap_or_else(|| "unverified".to_string());
    // The ledger line names the signal this start answers, inside the 200-character reason cap.
    let reason_for_log: String = match args.signal_id.as_deref() {
        Some(s) => format!("{} [signal {s}]", args.reason.trim()),
        None => args.reason.trim().to_string(),
    }
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
    // `--json`: one line on stdout, `{role,outcome,reason[,launch_to_live_secs]}`; the human
    // lines stay on stderr and the exit codes do not change.
    let emit = |outcome: &str, reason: &str, secs: Option<u64>| {
        if args.json {
            let mut v = serde_json::json!({
                "role": args.role, "outcome": outcome, "reason": reason,
            });
            if let Some(s) = secs {
                v["launch_to_live_secs"] = s.into();
            }
            println!("{v}");
        }
    };
    let req = start::StartRequest {
        role: &args.role,
        reason: &args.reason,
        model: args.model.as_deref(),
        signal_id: args.signal_id.as_deref(),
    };
    let plan = match start::plan_start(&state_dir(), &req, caller, &facts) {
        Ok(p) => p,
        Err(refusal) => {
            eprintln!("lane-restart start: refused - {refusal}");
            ledger(None, &format!("refused: {refusal}"), false);
            let (o, r) = refusal.outcome();
            emit(o, r, None);
            return ExitCode::from(refusal.exit_code());
        }
    };
    if let Err(refusal) = disk_gate("start") {
        ledger(None, &format!("refused: {refusal}"), false);
        emit("failed", "disk-floor", None);
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
        emit("dry-run", "dry-run", None);
        return ExitCode::SUCCESS;
    }
    eprintln!(
        "lane-restart start: launching {} on {}...",
        args.role, plan.model
    );
    let launched_at = std::time::Instant::now();
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
            // Seconds from spawn to confirmed progress, only when it was confirmed: an
            // awaiting-dialog wait is a person's time, not the launch's (spec C4 `launch_wait`).
            let (what, exit, secs, reason) = match done.outcome {
                relaunch::RelaunchOutcome::Relaunched => (
                    "started a fresh session (continuity via HANDOFF only)",
                    ExitCode::SUCCESS,
                    Some(launched_at.elapsed().as_secs()),
                    "started",
                ),
                relaunch::RelaunchOutcome::AwaitingConfirmation => (
                    "started; awaiting human confirmation at the dev-channels dialog",
                    ExitCode::from(2),
                    None,
                    "awaiting-dialog",
                ),
            };
            let live = secs
                .map(|s| format!(" launch-to-live:{s}s"))
                .unwrap_or_default();
            ledger(
                Some(&plan.model),
                &format!("{what}{pin_note}{warn}{live}"),
                true,
            );
            emit("started", reason, secs);
            exit
        }
        Err(e) => {
            eprintln!("lane-restart start: {e}");
            ledger(
                Some(&plan.model),
                &format!("action failed: {e}{pin_note}"),
                false,
            );
            emit("failed", "spawn-failed", None);
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
            let note = match &e {
                lane_stop::StopError::Kill(inner) => identity_note(inner, plan.state.pid),
                lane_stop::StopError::SpecNotWritten(_) => String::new(),
            };
            eprintln!("lane-restart stop: {e}{note}");
            ledger(
                Some(&plan.stopping.model),
                &format!("action failed: {e}{pin_note}{note}"),
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

/// `install --from <exe> [--dry-run]` -> `(exe, dry_run)`.
fn parse_install_args(rest: &[String]) -> Result<(PathBuf, bool), String> {
    let mut from = None;
    let mut dry_run = false;
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--from" => {
                from = Some(PathBuf::from(
                    iter.next().ok_or("--from needs a path")?.clone(),
                ))
            }
            "--dry-run" => dry_run = true,
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    let from = from.ok_or("--from <exe> is required")?;
    Ok((from, dry_run))
}

/// Runs the candidate for real: `--version`, then `status`.
struct RealProbe;

impl lane_restart::install::Probe for RealProbe {
    fn version_output(&self, exe: &std::path::Path) -> Result<String, String> {
        let out = std::process::Command::new(exe)
            .arg("--version")
            .output()
            .map_err(|e| format!("cannot run {}: {e}", exe.display()))?;
        if !out.status.success() {
            return Err(format!("exited {}", out.status));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn status_ok(&self, exe: &std::path::Path) -> Result<(), String> {
        let out = std::process::Command::new(exe)
            .arg("status")
            .output()
            .map_err(|e| format!("cannot run {}: {e}", exe.display()))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }
}

/// `lane-restart install --from <exe> [--dry-run]` - PM only, even as a dry run.
fn run_install_cmd(from: PathBuf, dry_run: bool) -> ExitCode {
    let facts = SysinfoFacts::new(claude_config_dir());
    let caller = verified_caller(&facts);
    let caller_role = caller.as_ref().ok().cloned();
    let requested_by = caller_role
        .clone()
        .unwrap_or_else(|| "unverified".to_string());
    let role = lane_restart::install::EXE_NAME;
    if caller_role.as_deref() != Some(start::PM_ROLE) {
        let why = match caller {
            Ok(r) => format!("only the PM session may install the binary; this is running under {r:?}"),
            Err(e) => format!(
                "only the PM session may install the binary, and the caller could not be verified: {e}"
            ),
        };
        eprintln!("lane-restart install: refused - {why}");
        log_ledger(
            &requested_by,
            role,
            "install",
            caller_role.as_deref(),
            None,
            None,
            &format!("refused: {why}"),
            false,
        );
        return ExitCode::FAILURE;
    }
    let hooks = lane_restart::install::hooks_dir();
    match lane_restart::install::install(&hooks, &from, &RealProbe, dry_run) {
        Ok(done) => {
            let kept = done
                .kept_as
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "nothing was installed before".to_string());
            if dry_run {
                println!(
                    "DRY RUN - nothing written. {} verified: lane-restart {}, sha256 {}; the installed exe would be kept as {kept}",
                    from.display(),
                    done.version,
                    done.sha256
                );
            } else {
                log_ledger(
                    &requested_by,
                    role,
                    "install",
                    caller_role.as_deref(),
                    None,
                    None,
                    &format!(
                        "installed {} sha256 {} from {}; previous kept as {kept}",
                        done.version,
                        done.sha256,
                        from.display()
                    ),
                    true,
                );
                println!(
                    "INSTALLED lane-restart {} at {}; sha256 {}; the previous exe is {kept}; {} rewritten",
                    done.version,
                    hooks.join(lane_restart::install::EXE_NAME).display(),
                    done.sha256,
                    hooks.join(lane_restart::install::SHA_NAME).display()
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("lane-restart install: {e}");
            log_ledger(
                &requested_by,
                role,
                "install",
                caller_role.as_deref(),
                None,
                None,
                &format!("{e}"),
                false,
            );
            ExitCode::FAILURE
        }
    }
}

/// `ledger [--role R] [--kind K] [--since T] [--acted|--refused] [--last N] [--json]`.
fn parse_ledger_args(
    rest: &[String],
) -> Result<(lane_restart::ledger::Filter, bool, Option<String>, bool), String> {
    let mut f = lane_restart::ledger::Filter::default();
    let mut json = false;
    let mut by_lane = false;
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
            "--by-lane" => by_lane = true,
            other => return Err(format!("unrecognised argument: {other}")),
        }
    }
    Ok((f, json, since_raw, by_lane))
}

/// `lane-restart ledger` - read-only: prints `restart.log` filtered. Writes nothing.
fn run_ledger_cmd(
    mut f: lane_restart::ledger::Filter,
    json: bool,
    since: Option<String>,
    by_lane: bool,
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
    if by_lane {
        print!("{}", ledger::render_by_lane(&shown, read.rows.len()));
        return ExitCode::SUCCESS;
    }
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
    lane-restart (--role <name> | --self) [--dry-run] [--yes]
        Restart a lane. --self restarts the CALLING lane (no idle check) and
        needs no --role: the role is LANE_ROLE, else the one role whose lane
        state records the current directory (--role overrides);
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

    lane-restart status [--json] [--quiet-over <secs>] [--peers]
        --quiet-over marks (QUIET>Ns; activity.quiet_over in JSON) each live
        role whose last hook write is older than <secs>. Opt-in, nothing is
        enforced and there is no default; the PM's recommended value is 120.
        Read-only: each role's state (running, suspended, crashed, never_run,
        unknown), whether its launch spec (<role>.launch.json, written before a
        restart's kill and at a confirmed launch) is complete, and whether it can
        be started again from it. RESTART-TOOL-DESIGN.md section 14.
        --peers adds, per role, the claude-peers sessions whose helper's PARENT is that
        role's live claude (never joined by cwd), the sessions no role claims, and a
        truncated summary. Read-only; a broker that cannot be read is
        unknown for every role, never absent.

    lane-restart attach --role <name> --watch
        Read-only: shows the screen of a running direct lane host and then its live
        output, until the host ends or you press Ctrl-C. Nothing you type reaches the
        lane. --watch is required; interactive attach does not exist. Linux/unix only.

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
        person. Exit codes: 0 started with progress confirmed, 2 started and
        awaiting the dialog (--json: one outcome line on stdout), 3 the lane is already running (nothing was
        launched: a no-op, safe to call again), 1 any other refusal or a
        failed launch.
        REFUSED (duplicate-risk) while the process table holds a
        `lane-restart host --role <name>` or a claude named for the role,
        whatever the state file says; the pids are printed. A restart
        refuses the same way for any such process other than its own
        session and that session's host. `status` marks such roles
        duplicate-risk.

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

    lane-restart install --from <exe> [--dry-run]
        PM only. Replace C:/Projects/.claude-hooks/lane-restart.exe with a
        freshly built one. The candidate must run `--version` (first line
        `lane-restart x.y.z`) and `status` (exit 0) BEFORE anything changes.
        The installed exe is renamed to lane-restart.exe.bak (the one .bak;
        the previous .bak is replaced), the candidate is copied in and read
        back, and lane-restart.sha256 is rewritten and read back. A failure
        after the rename puts the old exe back. --dry-run verifies and
        reports, writes nothing.

    lane-restart ledger [--role <name>] [--kind <kind>] [--since <time|30m|2h|1d>]
                        [--acted | --refused] [--last <n>] [--json | --by-lane]
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
            return match parse_status_args(rest) {
                Ok((json, quiet_over, peers)) => run_status_cmd(json, quiet_over, peers),
                Err(e) => {
                    eprintln!(
                        "lane-restart status: {e}; expected `status [--json] [--quiet-over <secs>] [--peers]`"
                    );
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
        if first == "install" {
            return match parse_install_args(rest) {
                Ok((from, dry_run)) => run_install_cmd(from, dry_run),
                Err(e) => {
                    eprintln!(
                        "lane-restart install: {e}; expected `install --from <exe> [--dry-run]`"
                    );
                    ExitCode::FAILURE
                }
            };
        }
        if first == "ledger" {
            return match parse_ledger_args(rest) {
                Ok((f, json, since, by_lane)) => run_ledger_cmd(f, json, since, by_lane),
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
        if first == "attach" {
            return run_attach_cmd(rest);
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
    // A bare `--self` (the documented form): the role is the caller's own, found the way
    // `assert-idle` finds it. An explicit `--role` is never second-guessed here.
    let role = match args.role.clone() {
        Some(r) => r,
        None => {
            let cwd = std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let claude_pid =
                lane_state_writer::claude_parent_pid(std::process::id(), &RealParentProcess::new())
                    .ok();
            match lane_restart::spec::resolve_self_role(
                &state_dir(),
                std::env::var("LANE_ROLE").ok().as_deref(),
                &cwd,
                claude_pid,
            ) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("lane-restart: --self without --role: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
    };

    let target = if args.target_self {
        Target::Myself {
            role: role.clone(),
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
        Target::Other { role: role.clone() }
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
            log_outcome(requested_by, &role, &format!("refused: {refusal}"), false);
            return ExitCode::FAILURE;
        }
    }
    let req = authorize::Request {
        target,
        confirmed: args.confirmed,
        dry_run: args.dry_run,
    };

    let decision = authorize::decide(&req, &facts, &state_dir());
    // The target's own session and its host are what this restart replaces; any OTHER host or
    // claude of the role is a second session the state file may not show (2026-10-14).
    if let Ok(plan) = &decision {
        let names = lane_restart::duplicate::role_names(&state_dir(), &role);
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        if let Err(refusal) =
            lane_restart::duplicate::check(&facts, &role, &names, Some(plan.state.pid))
        {
            eprintln!("lane-restart: refused - {refusal}");
            log_outcome(requested_by, &role, &format!("refused: {refusal}"), false);
            return ExitCode::FAILURE;
        }
    }
    match decision {
        Err(refusal) => {
            eprintln!("lane-restart: refused - {refusal}");
            log_outcome(requested_by, &role, &format!("refused: {refusal}"), false);
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
            log_outcome(requested_by, &role, "dry run - no action taken", false);
            ExitCode::SUCCESS
        }
        Ok(plan) => {
            // Before the kill: after it, a refusal would leave a lane dead and not relaunched.
            if let Err(refusal) = disk_gate("restart") {
                log_outcome(requested_by, &role, &format!("refused: {refusal}"), false);
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
                        &role,
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
                        &role,
                        "awaiting human confirmation at the dev-channels dialog",
                        true,
                    );
                    relaunch_exit_code(outcome)
                }
                Err(e) => {
                    let note = identity_note(&e, plan.state.pid);
                    eprintln!("lane-restart: {e}{note}");
                    log_outcome(
                        requested_by,
                        &role,
                        &format!("action failed: {e}{note}"),
                        false,
                    );
                    ExitCode::FAILURE
                }
            };
            close_leftover_tab(
                &facts,
                "restart",
                requested_by,
                &role,
                leftover_shell,
                relaunch_acted,
                plan.state.pid,
                &plan.identity,
            );
            exit
        }
    }
}

/// Issue #14 (`docs/PORT-LINUX-PLAN.md`, P3'): when a kill was refused because the pid's identity no
/// longer matched, append what `/proc` says about its start so the log can tell a stepped clock
/// from a recycled pid. Empty for every other error. Changes no decision.
fn identity_note(e: &relaunch::RelaunchError, pid: u32) -> String {
    match e {
        relaunch::RelaunchError::Kill(lane_restart::facts::KillError::IdentityChanged) => {
            format!(" ({})", lane_restart::platform_diag(pid))
        }
        _ => String::new(),
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
    fn a_bare_self_is_accepted_and_the_role_is_resolved_later() {
        // The documented form (`lane-restart --self`, CLAUDE.md and CIRESNAVE-EXPECTATIONS 2.3b).
        let a = parse_args(strs(&["--self"])).expect("bare --self must parse");
        assert!(a.target_self);
        // without --self the role is still required
        assert_eq!(
            parse_args(strs(&["--yes"])).unwrap_err(),
            ArgError::RoleRequired
        );
    }

    #[test]
    fn status_args_take_json_and_an_optional_positive_quiet_over_in_any_order() {
        assert_eq!(parse_status_args(&strs(&[])), Ok((false, None, false)));
        assert_eq!(
            parse_status_args(&strs(&["--json"])),
            Ok((true, None, false))
        );
        assert_eq!(
            parse_status_args(&strs(&["--quiet-over", "120", "--json"])),
            Ok((true, Some(120), false))
        );
        assert_eq!(
            parse_status_args(&strs(&["--peers", "--json"])),
            Ok((true, None, true))
        );
        for bad in [
            &["--quiet-over"][..],
            &["--quiet-over", "0"],
            &["--quiet-over", "-5"],
            &["--quiet-over", "2m"],
            &["--quiet-over", "1", "--quiet-over", "2"],
            &["--wat"],
        ] {
            assert!(parse_status_args(&strs(bad)).is_err(), "{bad:?}");
        }
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
                "--dry-run",
                "--json",
                "--signal-id",
                "baracuda:7"
            ])),
            Ok(StartArgs {
                role: "baracuda".into(),
                reason: "resume after crash".into(),
                model: Some("haiku".into()),
                dry_run: true,
                json: true,
                signal_id: Some("baracuda:7".into()),
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
    fn install_args_need_from_and_refuse_the_unknown() {
        assert!(parse_install_args(&[]).is_err());
        assert_eq!(
            parse_install_args(&strs(&["--from", "x.exe", "--dry-run"])),
            Ok((PathBuf::from("x.exe"), true))
        );
        assert_eq!(
            parse_install_args(&strs(&["--from", "x.exe"])),
            Ok((PathBuf::from("x.exe"), false))
        );
        assert!(parse_install_args(&strs(&["--from"])).is_err());
        assert!(parse_install_args(&strs(&["--from", "x.exe", "--yes"])).is_err());
        assert!(USAGE.contains("lane-restart install --from"));
    }

    #[test]
    fn ledger_args_parse_filters_and_refuse_the_unknown() {
        let (f, json, since, by_lane) = parse_ledger_args(&strs(&[
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
        assert!(!by_lane);
        let (_, _, _, by_lane) = parse_ledger_args(&strs(&["--by-lane"])).unwrap();
        assert!(by_lane);
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
