// SPDX-License-Identifier: MIT OR Apache-2.0

//! CLI entry point. All the actual decision logic lives in the library
//! (`authorize.rs`, `facts.rs`, `state.rs`, `log.rs`) so it can be unit
//! tested without a real process. This file only: parses argv, wires the
//! real `SystemFacts`, calls `authorize::decide`, and - only if it comes
//! back `Ok` with `will_act: true` - performs the kill and the relaunch.

use lane_restart::authorize::{self, Target};
use lane_restart::facts::SysinfoFacts;
use lane_restart::lane_state_writer::{self, RealParentProcess};
use lane_restart::log;
use lane_restart::relaunch;
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
        Ok(()) => ExitCode::SUCCESS,
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
        running. Required before any restart of that lane can be
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
    let requested_by = if args.target_self { "self" } else { "pm" };

    let facts = SysinfoFacts::new(claude_config_dir());
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
fn close_leftover_tab(
    facts: &SysinfoFacts,
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
            log_outcome(
                requested_by,
                role,
                &format!("closing the leftover tab: terminating shell {shell}"),
                true,
            );
        })
    });
    if let Err(why) = result {
        log_outcome(
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

fn log_outcome(requested_by: &str, role: &str, outcome: &str, acted: bool) {
    let entry = log::Entry {
        timestamp: chrono::Utc::now(),
        requested_by,
        role,
        outcome: outcome.to_string(),
        acted,
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
