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
use std::path::PathBuf;
use std::process::ExitCode;

/// `C:/Projects/.lane-state` - RESTART-TOOL-DESIGN.md §7. Not configurable
/// via CLI on purpose: a caller-supplied state directory would defeat the
/// whole point of a fixed, portfolio-wide location every lane and the PM
/// agree on.
fn state_dir() -> PathBuf {
    PathBuf::from("C:/Projects/.lane-state")
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
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
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
    let my_pid = std::process::id();
    let lane_role_env = std::env::var("LANE_ROLE").ok();
    let mut stdin = std::io::stdin();
    match lane_state_writer::run(
        &state_dir(),
        event,
        my_pid,
        &RealParentProcess,
        lane_role_env.as_deref(),
        &mut stdin,
    ) {
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
        &RealParentProcess,
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
/// version, plus every embedded handler's `id` and a hash of its exact
/// content, so anyone can see precisely what's active without reading
/// source.
fn print_version() {
    println!("lane-restart {}", env!("CARGO_PKG_VERSION"));
    if lane_restart::handlers::EMBEDDED_HANDLER_JSON.is_empty() {
        println!("embedded handlers: none");
        return;
    }
    println!("embedded handlers:");
    for json in lane_restart::handlers::EMBEDDED_HANDLER_JSON {
        let hash = lane_restart::handlers::handler_content_hash(json);
        match serde_json::from_str::<lane_restart::handlers::HandlerSpec>(json) {
            Ok(h) => println!("  {} {hash}", h.id),
            Err(e) => println!("  <unparseable: {e}> {hash}"),
        }
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
        (RESTART-TOOL-DESIGN.md §12.5). Owns a ConPTY, hosts <argv...>
        (normally `claude ...`) inside it, relays every byte
        transparently, and - only during the startup window - checks the
        screen against every embedded startup-prompt handler
        (RESTART-TOOL-DESIGN.md §12), injecting the matching one's action.
        Wired automatically by a restart; never run this by hand.

    lane-restart --version
        Print the crate version plus every embedded handler's id and a
        hash of its exact content (RESTART-TOOL-DESIGN.md §12.9).

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
            match relaunch::kill_and_relaunch(
                &facts,
                &state_reader,
                &plan.state,
                &plan.identity,
                &mut on_awaiting,
            ) {
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
            }
        }
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

/// The only place a real kill signal or a real process spawn happens.
/// Deliberately NOT covered by `authorize.rs`'s tests (which never touch a
/// real process) - `valid_identifier` and the argv SHAPE are unit tested
/// here; the actual OS calls are exercised by `--dry-run` against real
/// lanes before `--yes` is ever used for real.
mod relaunch {
    use lane_restart::facts::{KillError, ProcessIdentity, SystemFacts};
    use lane_restart::state::LaneState;

    /// PM finding, 2026-09-18: `role` and `name` come from a file the
    /// TARGET LANE wrote about itself, and reached a `cmd.exe /C start`
    /// command line - which parses metacharacters (`& | ^ % < >`) that
    /// Rust's own argv-passing quoting does not protect against, because
    /// it's cmd.exe doing a SECOND round of parsing on already-quoted
    /// arguments. Two independent fixes, not one: this validator refuses
    /// anything that isn't a plain identifier, AND (below) the launch no
    /// longer goes through cmd.exe at all - `CREATE_NEW_CONSOLE` spawns
    /// `claude.exe` directly with a real argv array, which Windows'
    /// `CreateProcess` never hands to a shell for re-parsing. Either fix
    /// alone would have closed this; both together don't depend on staying
    /// right about which one actually mattered.
    pub fn valid_identifier(s: &str) -> bool {
        !s.is_empty()
            && s.len() <= 64
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }

    #[derive(Debug)]
    pub enum RelaunchError {
        InvalidIdentifier(String),
        /// PM finding, 2026-09-18 (second real-restart retest): `wt.exe`
        /// treats `;` as ITS OWN command separator (`wt new-tab ; split-pane
        /// ...`), a parsing layer on top of the normal, safe argv passing
        /// `CreateProcess` already does. A `;` in any argv element this
        /// module builds - `cwd`, `model`, `permission_mode`, the prompt -
        /// would be read as a second `wt` command, not as literal text.
        UnsafeArgument(String),
        Kill(KillError),
        Spawn(String),
        /// PM finding, 2026-09-18 (fifth real-restart retest, revised
        /// spec): the process died, or never came up at all, within
        /// `TOTAL_TIMEOUT` - not the "awaiting a human's confirmation at a
        /// known dialog" case, which is `RelaunchOutcome::AwaitingConfirmation`
        /// instead (a real, expected outcome, never an error).
        SessionNeverProcessedPrompt,
    }

    /// What `kill_and_relaunch` actually achieved - `Relaunched` is the
    /// only fully-done outcome; `AwaitingConfirmation` is real, ACTED work
    /// (the kill and the relaunch both happened) that isn't finished
    /// because a human still needs to act, never an error.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum RelaunchOutcome {
        /// The new session's own state file showed a `UserPromptSubmit` (or
        /// later) event under its OWN fresh `session_id` - it's genuinely
        /// working, not just alive.
        Relaunched,
        /// PM finding, 2026-09-18 (fifth real-restart retest, CireSnave via
        /// the PM): `--dangerously-load-development-channels` shows a
        /// security confirmation dialog on EVERY start (never auto-
        /// answered - it's a security prompt). A relaunched lane that
        /// carries this flag (kept in the allowlist - it's the only way
        /// non-Claude agents reach a lane at all, per `channels-reference`'s
        /// own "no bypass during the research preview") can be stuck at
        /// exactly that dialog: alive, genuinely running, doing nothing
        /// until a human confirms. The OLD (process-alive-only) check
        /// would have logged this as success; this is why liveness now
        /// requires the state file to show real progress, not just a live
        /// process.
        AwaitingConfirmation,
    }

    impl std::fmt::Display for RelaunchError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                RelaunchError::InvalidIdentifier(what) => {
                    write!(f, "refusing to launch - not a plain identifier: {what}")
                }
                RelaunchError::UnsafeArgument(what) => {
                    write!(
                        f,
                        "refusing to launch - unsafe for wt.exe's own argument parser: {what}"
                    )
                }
                RelaunchError::Kill(e) => write!(f, "kill refused: {e}"),
                RelaunchError::Spawn(e) => write!(f, "could not launch the relaunch command: {e}"),
                RelaunchError::SessionNeverProcessedPrompt => {
                    write!(f, "relaunch FAILED: session never processed its prompt")
                }
            }
        }
    }

    /// The `claude` argv this module launches, everywhere - not a command
    /// line string, an actual argument list. ⚠️ NEVER `--resume`. PM
    /// finding, 2026-09-18: resuming reloads the whole prior transcript,
    /// carrying the full context back in - exactly the per-turn cost a
    /// restart exists to cut. `state.session_id` is used only by
    /// `authorize::decide`'s identity check (RESTART-TOOL-DESIGN.md §2),
    /// never here.
    /// How many values immediately following a flag belong to it, when
    /// reparsing `state.launch_args` (the ORIGINAL launch's real argv) to
    /// decide what a relaunch carries over.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FlagArity {
        /// Takes zero values, e.g. `--remote-control`.
        None,
        /// Takes exactly one value, e.g. `--mcp-config <file>`.
        One,
        /// Takes zero or one value: the next token IF it doesn't itself
        /// look like a flag, e.g. `--resume` (bare, or `--resume <id>`).
        OptionalOne,
        /// Takes one or more values, consumed until the next `-`-prefixed
        /// token, e.g. `--add-dir <dir> <dir> ...`.
        Variadic,
    }

    /// Flags carried over verbatim from the original launch, on top of
    /// `--model`/`--permission-mode`/`--remote-control` (which stay
    /// governed by their OWN state fields with their own "don't invent a
    /// value"/fallback rules - see `state.rs`, and `--dangerously-skip-
    /// permissions`, which is already represented there as
    /// `permission_mode: Some("bypassPermissions")`, so re-emitting the
    /// original flag literally here would just be a redundant restatement
    /// of the same fact).
    ///
    /// ⚠️ `--dangerously-load-development-channels` KEPT here (CireSnave,
    /// via the PM, 2026-09-18, correcting a same-day retraction): it's the
    /// only way non-Claude agents (Synapse, FAM, `claude-peers`) can reach
    /// a lane at all - `channels-reference`'s own docs confirm there is NO
    /// bypass during the research preview, and `--channels` only accepts
    /// Anthropic-allowlisted plugins, so it's not a substitute for a local
    /// MCP server like `claude-peers`. **It DOES show a security
    /// confirmation dialog on every start, never auto-answered** - that's
    /// what `wait_for_relaunch_liveness`'s `AwaitingConfirmation` outcome
    /// exists to detect and report, not something this allowlist should
    /// paper over by dropping the flag.
    const ALLOWED_LAUNCH_ARG_FLAGS: &[(&str, FlagArity)] = &[
        (
            "--dangerously-load-development-channels",
            FlagArity::Variadic,
        ),
        ("--add-dir", FlagArity::Variadic),
        ("--mcp-config", FlagArity::One),
        ("--settings", FlagArity::One),
    ];

    /// Flags known to drop, with the arity to skip PAST correctly - so a
    /// dropped flag's own value is never misread as a stray positional or
    /// as a value belonging to a DIFFERENT flag. `--name` is included here
    /// deliberately: `claude_argv` already emits a fresh `--name` itself,
    /// so dropping the original is what keeps it from being emitted twice.
    const DROPPED_LAUNCH_ARG_FLAGS: &[(&str, FlagArity)] = &[
        ("--resume", FlagArity::OptionalOne),
        ("-r", FlagArity::OptionalOne),
        ("--continue", FlagArity::None),
        ("-c", FlagArity::None),
        ("-p", FlagArity::None),
        ("--print", FlagArity::None),
        ("--session-id", FlagArity::OptionalOne),
        ("--fork-session", FlagArity::None),
        ("--name", FlagArity::One),
        // Recognised-and-intentionally-dropped, not "unknown": these are
        // already carried over by claude_argv itself, from state.model /
        // state.permission_mode / state.remote_control - each with its own
        // "don't invent a value"/fallback rule (state.rs), richer than a
        // blind re-parse of the original argv would give. Re-emitting the
        // literal original flag here would just duplicate that, or (for
        // --dangerously-skip-permissions) restate a fact state.permission_mode
        // already represents as `Some("bypassPermissions")`.
        ("--model", FlagArity::One),
        ("--permission-mode", FlagArity::One),
        ("--remote-control", FlagArity::None),
        ("--dangerously-skip-permissions", FlagArity::None),
    ];

    fn is_flag_token(s: &str) -> bool {
        s.starts_with('-') && s != "-"
    }

    /// Reparses `launch_args` (the ORIGINAL launch's real argv, program
    /// name included) into the extra argv elements a relaunch should carry
    /// over - an ALLOWLIST, never a blind pass-through - plus the names of
    /// every unrecognised flag it dropped, so the caller can log them
    /// rather than silently discard them.
    /// Advances `iter` past the value(s) belonging to a flag of the given
    /// `arity`, pushing each consumed value onto `sink` (an empty no-op
    /// sink drops them instead of carrying them over).
    fn consume_flag_values<'a, I>(
        arity: FlagArity,
        iter: &mut std::iter::Peekable<I>,
        sink: &mut Vec<String>,
    ) where
        I: Iterator<Item = &'a String>,
    {
        match arity {
            FlagArity::None => {}
            FlagArity::One => {
                if let Some(v) = iter.next() {
                    sink.push(v.clone());
                }
            }
            FlagArity::OptionalOne => {
                if iter.peek().is_some_and(|v| !is_flag_token(v)) {
                    sink.push(iter.next().unwrap().clone());
                }
            }
            FlagArity::Variadic => {
                while let Some(v) = iter.peek() {
                    if is_flag_token(v) {
                        break;
                    }
                    sink.push(iter.next().unwrap().clone());
                }
            }
        }
    }

    /// Reparses `launch_args` (the ORIGINAL launch's real argv, program
    /// name included) into the extra argv elements a relaunch should carry
    /// over - an ALLOWLIST, never a blind pass-through - plus the names of
    /// every unrecognised flag it dropped, so the caller can log them
    /// rather than silently discard them.
    fn extra_launch_args(launch_args: &[String]) -> (Vec<String>, Vec<String>) {
        let mut allowed = Vec::new();
        let mut dropped_unknown = Vec::new();
        let mut iter = launch_args.iter().skip(1).peekable();
        while let Some(token) = iter.next() {
            // `--permission-mode=value` is already carried over via
            // state.permission_mode (see DROPPED_LAUNCH_ARG_FLAGS's own
            // comment) - the equals form embeds its value in the same
            // token, so it never matches an exact `--permission-mode`
            // lookup below and would otherwise be misread as unknown.
            if token.starts_with("--permission-mode=") {
                continue;
            }
            if let Some((_, arity)) = ALLOWED_LAUNCH_ARG_FLAGS.iter().find(|(f, _)| f == token) {
                allowed.push(token.clone());
                consume_flag_values(*arity, &mut iter, &mut allowed);
                continue;
            }
            if let Some((_, arity)) = DROPPED_LAUNCH_ARG_FLAGS.iter().find(|(f, _)| f == token) {
                let mut discard = Vec::new();
                consume_flag_values(*arity, &mut iter, &mut discard);
                continue;
            }
            if is_flag_token(token) {
                // Unknown arity: drop only the flag itself - assuming it
                // takes a value it might not have would risk eating a
                // DIFFERENT, real flag right after it. Never passed
                // through blindly.
                dropped_unknown.push(token.clone());
            }
            // A bare positional (the original prompt, if any) is silently
            // dropped - a relaunch always supplies its own fresh prompt.
        }
        (allowed, dropped_unknown)
    }

    /// Whether `launch_args` (the ORIGINAL launch's real argv) carried
    /// `--dangerously-load-development-channels` - the one flag known to
    /// show a security confirmation dialog on every start. Used only to
    /// decide whether a stuck-at-20s process is plausibly AWAITING a human
    /// confirmation, never to decide whether to carry the flag over (that
    /// decision is `ALLOWED_LAUNCH_ARG_FLAGS`'s alone).
    fn has_dev_channels_flag(launch_args: &[String]) -> bool {
        launch_args
            .iter()
            .any(|a| a == "--dangerously-load-development-channels")
    }

    /// ⚠️ PM finding, 2026-09-19 (real-rerun, "must be tagged" error from
    /// claude itself): `--dangerously-load-development-channels` is
    /// VARIADIC - it consumes every following non-flag token, including a
    /// trailing positional prompt, which is exactly what happened when the
    /// prompt was appended LAST: `claude` parsed it as a second (invalid)
    /// channel entry and exited before ever reaching the dialog. Fixed by
    /// putting the prompt FIRST, immediately after `claude` - nothing can
    /// ever directly follow ANY variadic flag's values again, since the
    /// variadic flags (from `extra_launch_args`) are always the LAST
    /// elements this function appends. **Verified against the real `claude`
    /// CLI** (a real spawn under `portable-pty`, no visible window, killed
    /// within a few seconds - matching this crate's real-process discipline
    /// elsewhere): the old (prompt-last) shape reproduces the exact
    /// "--dangerously-load-development-channels entries must be tagged"
    /// error verbatim; prompt-first parses cleanly and reaches claude's own
    /// first-run folder-trust dialog. See
    /// `no_positional_ever_directly_follows_a_variadic_flags_values` for the
    /// general property this now guarantees for every allowlisted variadic
    /// flag, not just this one.
    fn claude_argv(name: &str, state: &LaneState, prompt: &str) -> Vec<String> {
        let mut argv = vec!["claude".to_string(), prompt.to_string()];
        argv.push("--name".to_string());
        argv.push(name.to_string());
        if let Some(model) = &state.model {
            argv.push("--model".to_string());
            argv.push(model.clone());
        }
        if let Some(permission_mode) = &state.permission_mode {
            argv.push("--permission-mode".to_string());
            argv.push(permission_mode.clone());
        }
        if state.remote_control {
            argv.push("--remote-control".to_string());
        }
        if let Some(launch_args) = &state.launch_args {
            let (extra, dropped_unknown) = extra_launch_args(launch_args);
            argv.extend(extra);
            if !dropped_unknown.is_empty() {
                eprintln!(
                    "lane-restart: dropping unrecognised launch flag(s), not carrying them \
                     over to the relaunch: {}",
                    dropped_unknown.join(", ")
                );
            }
        }
        argv
    }

    /// PM finding, 2026-09-18: `wt.exe`'s own argument parser reads `;` as
    /// a command separator, independent of and in addition to the normal
    /// (already-safe) argv passing every element here goes through. Every
    /// element that reaches `wt.exe` - including `cwd`, which is passed as
    /// its own `-d` argument - must be checked, not just the ones this
    /// module builds itself.
    fn first_unsafe_argument<'a>(elements: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
        elements.into_iter().find(|e| e.contains(';'))
    }

    /// PM finding, 2026-09-18 (second real-restart retest, real end-to-end
    /// proof via HANDOFF continuity): launching `claude.exe` directly, even
    /// under `CREATE_NEW_CONSOLE`, still inherited this process's own std
    /// handles (Rust's `Command` always sets `STARTF_USESTDHANDLES`) - a
    /// hook-invoked lane runs from its own Bash tool, so the child got
    /// pipes, not a real console, and non-TTY stdin plus a prompt argument
    /// made `claude` behave like one-shot print mode: it read HANDOFF,
    /// replied, and exited 12s later. Fixed: launch through Windows
    /// Terminal, which gives the child a REAL ConPTY independent of this
    /// process's own handles - exactly how CireSnave's own lanes are
    /// launched (WindowsTerminal → pwsh → claude). Falls back to
    /// `conhost.exe` if `wt.exe` isn't on PATH.
    /// ⚠️ NOT UNIT TESTED HERE - same category as `SysinfoFacts`/
    /// `RealParentProcess` elsewhere in this crate: it spawns a real OS
    /// process (`wt.exe`, or `conhost.exe` as the fallback), which this
    /// crate must never do just to test itself. `claude_argv` (the argv it
    /// builds) and `first_unsafe_argument` (the check every element of
    /// that argv, plus `cwd`, passes before reaching this function) are
    /// both unit tested directly; this function's own fallback branching
    /// is exercised by real use, the same way a real restart is the
    /// acceptance check for the rest of §5.
    /// PM finding, 2026-09-18 (third real-restart retest): `lane-restart`
    /// itself always runs from a lane's own Bash tool, i.e. FROM INSIDE a
    /// running Claude Code session - `wt.exe` inherits that whole
    /// environment by default, so the "fresh" relaunch came up believing
    /// it was a CHILD of the session that requested the restart
    /// (`CLAUDE_CODE_CHILD_SESSION` inherited): no transcript, and the
    /// positional prompt never auto-submitted. Confirmed live by dumping
    /// `env` from inside a real session (not guessed) - only vars that
    /// actually name THIS session or its IPC channel are stripped; a
    /// user's own persistent config vars (`CLAUDE_EFFORT`,
    /// `CLAUDE_CODE_USE_POWERSHELL_TOOL`, `CLAUDE_CODE_EXECPATH`, and
    /// anything unrelated like `CLOUDFLARE_*`) are left alone.
    const SESSION_IDENTITY_ENV_VARS: &[&str] = &[
        "CLAUDECODE",
        "CLAUDE_CODE_CHILD_SESSION",
        "CLAUDE_CODE_ENTRYPOINT",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_CODE_SESSION_ATTENDED",
        "CLAUDE_CODE_BRIDGE_SESSION_ID",
        "CLAUDE_CODE_MESSAGING_SOCKET",
        "CLAUDE_CODE_MESSAGING_TOKEN",
        "CLAUDE_CODE_SSE_PORT",
        "CLAUDE_PID",
    ];

    fn strip_session_identity_env(cmd: &mut std::process::Command) {
        for var in SESSION_IDENTITY_ENV_VARS {
            cmd.env_remove(var);
        }
    }

    /// RESTART-TOOL-DESIGN.md §12.5: the tab `wt.exe` opens must run
    /// `lane-restart host`, never `claude` directly - the host is what
    /// owns the real ConPTY `claude` runs inside, observes it for a
    /// startup-prompt handler match during the startup window, and relays
    /// every byte transparently the rest of the time. `current_exe()` is
    /// used (rather than the bare name `lane-restart`) so the exact same
    /// binary that decided to relaunch is the one that hosts it, without
    /// depending on PATH resolving to the same install this process itself
    /// runs from.
    fn host_wrapped_argv(role: &str, claude_argv: &[String]) -> Vec<String> {
        let exe = std::env::current_exe()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "lane-restart".to_string());
        let mut argv = vec![
            exe,
            "host".to_string(),
            "--role".to_string(),
            role.to_string(),
            "--".to_string(),
        ];
        argv.extend_from_slice(claude_argv);
        argv
    }

    fn spawn_relaunch(state: &LaneState, argv: &[String]) -> Result<(), RelaunchError> {
        let hosted_argv = host_wrapped_argv(&state.role, argv);
        let mut wt = std::process::Command::new("wt.exe");
        wt.args(["-w", "new", "-d", &state.cwd]);
        wt.args(&hosted_argv);
        strip_session_identity_env(&mut wt);
        match wt.spawn() {
            Ok(_) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // fall through to the conhost.exe fallback below
            }
            Err(e) => return Err(RelaunchError::Spawn(e.to_string())),
        }

        let mut conhost = std::process::Command::new("conhost.exe");
        conhost.args(&hosted_argv);
        conhost.current_dir(&state.cwd);
        strip_session_identity_env(&mut conhost);
        conhost
            .spawn()
            .map(|_| ())
            .map_err(|e| RelaunchError::Spawn(e.to_string()))
    }

    /// Reads the target role's OWN current state file - injected so the
    /// liveness check is testable without real file I/O, the same way
    /// `facts: &dyn SystemFacts` makes process facts testable without a
    /// real process.
    pub trait StateReader {
        fn read(&self, role: &str) -> Option<LaneState>;
    }

    pub struct RealStateReader<'a> {
        pub state_dir: &'a std::path::Path,
    }

    impl StateReader for RealStateReader<'_> {
        fn read(&self, role: &str) -> Option<LaneState> {
            lane_restart::state::load(self.state_dir, role).ok()
        }
    }

    /// PM finding, 2026-09-18 (fourth AND fifth real-restart retests): a
    /// live process is not a WORKING session -
    /// `--dangerously-load-development-channels` shows a security
    /// confirmation dialog on every start that a relaunched lane can sit
    /// at, alive and doing nothing, until a human confirms it - never
    /// auto-answered, since it's a security prompt. **Three outcomes, not
    /// two:**
    ///
    /// - **`Relaunched`**: the target role's OWN state file shows a
    ///   `session_id` that differs from `old_session_id` (this IS the
    ///   fresh session) whose `updated_by_event` is something AFTER
    ///   `SessionStart` - real progress, not just existence (a session
    ///   stuck at the dialog still fires `SessionStart`, then never gets
    ///   past it).
    /// - **`AwaitingConfirmation`**: no progress within `PROGRESS_TIMEOUT`
    ///   (~20s), but the process IS alive AND its own launch carried the
    ///   dev-channels flag - exactly the dialog shape. `on_awaiting` fires
    ///   ONCE, immediately, the moment this is first detected (never at the
    ///   very end) - real callers use it to notify a human promptly, since
    ///   confirming the dialog is the only way forward. Polling then
    ///   CONTINUES up to `TOTAL_TIMEOUT` (~15 min): a human confirming
    ///   flips the outcome to `Relaunched`; reaching the full timeout still
    ///   stuck returns `AwaitingConfirmation` as the FINAL outcome (real,
    ///   acted work - the kill and relaunch both genuinely happened - not
    ///   an error).
    /// - **`Err(SessionNeverProcessedPrompt)`**: the process died, or never
    ///   came up, before `PROGRESS_TIMEOUT` with no dev-channels flag to
    ///   explain the stall - or died at any point thereafter.
    ///
    /// `sleep` is injected so this whole polling protocol (including the
    /// ~15 minute total) is testable without a real wait; `on_awaiting` is
    /// injected so a test can assert it fired at most once, at the right
    /// moment, without capturing real stdout.
    #[allow(clippy::too_many_arguments)]
    fn wait_for_relaunch_liveness(
        facts: &dyn SystemFacts,
        state_reader: &dyn StateReader,
        cwd: &str,
        role: &str,
        old_session_id: &str,
        killed_at_secs: u64,
        carries_dev_channels_flag: bool,
        sleep: &mut dyn FnMut(std::time::Duration),
        on_awaiting: &mut dyn FnMut(),
    ) -> Result<RelaunchOutcome, RelaunchError> {
        const PROGRESS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
        // ⚠️ "Configurable" per the PM's own spec - not yet a CLI flag;
        // this constant is the one place to change it until it is.
        const TOTAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15 * 60);
        const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

        let mut waited = std::time::Duration::ZERO;
        let mut reported_awaiting = false;
        loop {
            if let Some(state) = state_reader.read(role) {
                if state.session_id != old_session_id && state.updated_by_event != "SessionStart" {
                    return Ok(RelaunchOutcome::Relaunched);
                }
            }
            if waited >= PROGRESS_TIMEOUT {
                let alive = facts.find_claude_process_in(cwd, killed_at_secs).is_some();
                if !alive {
                    return Err(RelaunchError::SessionNeverProcessedPrompt);
                }
                if !reported_awaiting {
                    if !carries_dev_channels_flag {
                        // Alive, no progress, and nothing known to explain
                        // the stall - not the dialog case. Keep polling to
                        // TOTAL_TIMEOUT anyway (a slow session is still a
                        // real possibility), but never report a confirmation
                        // that has no evidence behind it.
                    } else {
                        on_awaiting();
                        reported_awaiting = true;
                    }
                }
            }
            if waited >= TOTAL_TIMEOUT {
                return if reported_awaiting {
                    Ok(RelaunchOutcome::AwaitingConfirmation)
                } else {
                    Err(RelaunchError::SessionNeverProcessedPrompt)
                };
            }
            sleep(POLL_INTERVAL);
            waited += POLL_INTERVAL;
        }
    }

    /// PM finding, 2026-09-19: the dry-run message used to rebuild its own
    /// string by hand, separately from the argv the real launch actually
    /// uses - and silently dropped the carried-over `launch_args` flags
    /// (`--dangerously-load-development-channels` included) doing so. This
    /// calls the EXACT SAME `claude_argv` the real launch calls, so the two
    /// can never drift apart again.
    pub fn describe_dry_run(state: &LaneState) -> Result<Vec<String>, RelaunchError> {
        let name = state.name.as_deref().unwrap_or(&state.role);
        if !valid_identifier(&state.role) {
            return Err(RelaunchError::InvalidIdentifier(format!(
                "role {:?}",
                state.role
            )));
        }
        if !valid_identifier(name) {
            return Err(RelaunchError::InvalidIdentifier(format!("name {name:?}")));
        }
        let prompt = format!("read {} HANDOFF and continue", state.role);
        Ok(claude_argv(name, state, &prompt))
    }

    pub fn kill_and_relaunch(
        facts: &dyn SystemFacts,
        state_reader: &dyn StateReader,
        state: &LaneState,
        identity: &ProcessIdentity,
        on_awaiting: &mut dyn FnMut(),
    ) -> Result<RelaunchOutcome, RelaunchError> {
        let name = state.name.as_deref().unwrap_or(&state.role);
        if !valid_identifier(&state.role) {
            return Err(RelaunchError::InvalidIdentifier(format!(
                "role {:?}",
                state.role
            )));
        }
        if !valid_identifier(name) {
            return Err(RelaunchError::InvalidIdentifier(format!("name {name:?}")));
        }

        let prompt = format!("read {} HANDOFF and continue", state.role);
        let argv = claude_argv(name, state, &prompt);
        if let Some(bad) = first_unsafe_argument(
            std::iter::once(state.cwd.as_str()).chain(argv.iter().map(String::as_str)),
        ) {
            return Err(RelaunchError::UnsafeArgument(bad.to_string()));
        }
        // ⚠️ PM finding, 2026-09-19 (real-restart test): must check the argv
        // this call is ACTUALLY ABOUT TO LAUNCH, not re-derive a guess from
        // old `state.launch_args` - `argv` is the single source of truth for
        // what's really being carried over (it already went through
        // `extra_launch_args`'s allowlist, which `state.launch_args` alone
        // doesn't reflect), and checking it directly can never disagree with
        // what actually gets launched two lines below.
        let carries_dev_channels_flag = has_dev_channels_flag(&argv);

        facts
            .kill_verified(state.pid, identity)
            .map_err(RelaunchError::Kill)?;
        // Give the OS a moment to finish tearing the process down before a
        // new `claude` process claims the same working directory's lock.
        std::thread::sleep(std::time::Duration::from_millis(500));
        // ⚠️ A SMALL SAFETY MARGIN, not a guess: `facts.now()` is wall-clock
        // time, but `find_claude_process_in`'s `start_time` comes from the
        // OS (on Linux, ticks-since-boot converted to a Unix timestamp) -
        // two different clock sources that can disagree by a second or two
        // without either being "wrong" - confirmed live, this crate's own
        // real-process liveness test failed on a CI runner for exactly this
        // reason before the margin was added.
        let killed_at_secs = facts.now().timestamp().max(0).saturating_sub(5) as u64;

        spawn_relaunch(state, &argv)?;

        wait_for_relaunch_liveness(
            facts,
            state_reader,
            &state.cwd,
            &state.role,
            &state.session_id,
            killed_at_secs,
            carries_dev_channels_flag,
            &mut std::thread::sleep,
            on_awaiting,
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn strs(args: &[&str]) -> Vec<String> {
            args.iter().map(|s| s.to_string()).collect()
        }

        #[test]
        fn a_plain_role_name_is_valid() {
            assert!(valid_identifier("overmind"));
            assert!(valid_identifier("pm-2"));
            assert!(valid_identifier("lane_42"));
        }

        #[test]
        fn shell_metacharacters_are_refused() {
            assert!(!valid_identifier("a&calc"));
            assert!(!valid_identifier("a|calc"));
            assert!(!valid_identifier("a^calc"));
            assert!(!valid_identifier("a%PATH%"));
            assert!(!valid_identifier("a<calc"));
            assert!(!valid_identifier("a>calc"));
            assert!(!valid_identifier("a;calc"));
            assert!(!valid_identifier("a calc"));
        }

        #[test]
        fn empty_and_oversized_are_refused() {
            assert!(!valid_identifier(""));
            assert!(!valid_identifier(&"x".repeat(65)));
            assert!(valid_identifier(&"x".repeat(64)));
        }

        #[test]
        fn dots_and_slashes_are_refused_too() {
            // Not shell metacharacters, but still not a plain identifier -
            // a role/name is never expected to need them.
            assert!(!valid_identifier("a.b"));
            assert!(!valid_identifier("a/b"));
            assert!(!valid_identifier("../etc"));
        }

        struct NeverCalled;
        impl SystemFacts for NeverCalled {
            fn is_alive_claude_process(&self, _: u32) -> bool {
                panic!("must not be reached")
            }
            fn cwd_of(&self, _: u32) -> Option<std::path::PathBuf> {
                panic!("must not be reached")
            }
            fn has_live_shell_descendant(
                &self,
                _: u32,
            ) -> Result<bool, lane_restart::facts::ShellCheckError> {
                panic!("must not be reached")
            }
            fn transcript_is_recent(&self, _: &str, _: &str, _: std::time::Duration) -> bool {
                panic!("must not be reached")
            }
            fn now(&self) -> chrono::DateTime<chrono::Utc> {
                panic!("must not be reached")
            }
            fn process_identity(&self, _: u32) -> Option<ProcessIdentity> {
                panic!("must not be reached")
            }
            fn kill_verified(&self, _: u32, _: &ProcessIdentity) -> Result<(), KillError> {
                panic!(
                    "kill_and_relaunch must refuse an invalid role/name BEFORE ever \
                     attempting to kill anything"
                )
            }
            fn find_claude_process_in(&self, _: &str, _: u64) -> Option<u32> {
                panic!("must not be reached")
            }
        }

        struct NeverCalledStateReader;
        impl StateReader for NeverCalledStateReader {
            fn read(&self, _role: &str) -> Option<LaneState> {
                panic!("must not be reached")
            }
        }

        fn state_with_role(role: &str) -> LaneState {
            LaneState {
                role: role.to_string(),
                session_id: "s".to_string(),
                pid: 1,
                cwd: "C:/x".to_string(),
                name: None,
                model: Some("claude-sonnet-5".to_string()),
                permission_mode: Some("prompting".to_string()),
                remote_control: false,
                busy: false,
                subagents_running: 0,
                no_background_shells: Some(true),
                launch_args: None,
                updated_at: chrono::Utc::now(),
                updated_by_event: "Stop".to_string(),
            }
        }

        fn dummy_identity() -> ProcessIdentity {
            ProcessIdentity {
                start_time_secs: 0,
                exe: None,
            }
        }

        #[test]
        fn kill_and_relaunch_refuses_an_invalid_role_before_touching_the_process() {
            // ⚠️ THE MUTATION THIS TEST EXISTS TO CATCH: it is not enough
            // for `valid_identifier` to be correct in isolation if
            // `kill_and_relaunch` doesn't actually call it as a gate.
            let state = state_with_role("a&calc");
            let result = kill_and_relaunch(
                &NeverCalled,
                &NeverCalledStateReader,
                &state,
                &dummy_identity(),
                &mut || {},
            );
            assert!(matches!(result, Err(RelaunchError::InvalidIdentifier(_))));
        }

        #[test]
        fn kill_and_relaunch_refuses_an_invalid_name_before_touching_the_process() {
            let mut state = state_with_role("overmind");
            state.name = Some("a|calc".to_string());
            let result = kill_and_relaunch(
                &NeverCalled,
                &NeverCalledStateReader,
                &state,
                &dummy_identity(),
                &mut || {},
            );
            assert!(matches!(result, Err(RelaunchError::InvalidIdentifier(_))));
        }

        // -- strip_session_identity_env ------------------------------------ //
        // PM finding, 2026-09-18 (third real-restart retest): `wt.exe`
        // inherits this process's own env by default, so a relaunch run
        // from inside a real session came up believing it was a CHILD of
        // that session (CLAUDE_CODE_CHILD_SESSION inherited) - no
        // transcript, prompt never auto-submitted.

        #[test]
        fn strip_session_identity_env_removes_every_listed_var() {
            let mut cmd = std::process::Command::new("does-not-matter");
            for var in SESSION_IDENTITY_ENV_VARS {
                cmd.env(var, "1");
            }
            cmd.env("UNRELATED_VAR", "keep-me");

            strip_session_identity_env(&mut cmd);

            let envs: std::collections::HashMap<_, _> = cmd.get_envs().collect();
            for var in SESSION_IDENTITY_ENV_VARS {
                assert_eq!(
                    envs.get(std::ffi::OsStr::new(var)),
                    Some(&None),
                    "{var} must be explicitly removed (env_remove), not merely left unset"
                );
            }
            assert_eq!(
                envs.get(std::ffi::OsStr::new("UNRELATED_VAR")),
                Some(&Some(std::ffi::OsStr::new("keep-me"))),
                "an unrelated var must be left alone - this isn't a blanket env wipe"
            );
        }

        // -- host_wrapped_argv --------------------------------------------- //
        // RESTART-TOOL-DESIGN.md §12.5: the tab wt.exe opens must run
        // `lane-restart host --role <r> -- <claude argv...>`, never
        // `claude` directly.

        #[test]
        fn host_wrapped_argv_puts_role_and_separator_before_the_claude_argv() {
            let claude = strs(&["claude", "--name", "overmind", "the prompt"]);
            let wrapped = host_wrapped_argv("overmind", &claude);
            let sep = wrapped.iter().position(|a| a == "--").expect("no -- found");
            assert_eq!(wrapped[sep + 1..], claude[..], "got {wrapped:?}");
            assert_eq!(wrapped[sep - 2], "--role");
            assert_eq!(wrapped[sep - 1], "overmind");
            assert_eq!(
                wrapped[0],
                std::env::current_exe().unwrap().to_string_lossy(),
                "the exe element must be THIS process's own current_exe(), not a bare name"
            );
        }

        #[test]
        fn host_wrapped_argv_contains_host_subcommand() {
            let wrapped = host_wrapped_argv("overmind", &strs(&["claude"]));
            assert_eq!(wrapped[1], "host");
        }

        // -- claude_argv ------------------------------------------------- //

        #[test]
        fn claude_argv_omits_model_and_permission_mode_when_absent() {
            let mut state = state_with_role("overmind");
            state.model = None;
            state.permission_mode = None;
            let argv = claude_argv("overmind", &state, "the prompt");
            assert_eq!(
                argv,
                vec!["claude", "the prompt", "--name", "overmind"]
                    .into_iter()
                    .map(String::from)
                    .collect::<Vec<_>>()
            );
        }

        #[test]
        fn claude_argv_includes_remote_control_flag_when_set() {
            let mut state = state_with_role("overmind");
            state.remote_control = true;
            let argv = claude_argv("overmind", &state, "p");
            assert!(argv.contains(&"--remote-control".to_string()));
        }

        #[test]
        fn claude_argv_never_includes_resume() {
            // ⚠️ PM finding, 2026-09-18: --resume reloads the whole prior
            // transcript - exactly the cost a restart exists to cut.
            let state = state_with_role("overmind");
            let argv = claude_argv("overmind", &state, "p");
            assert!(!argv.iter().any(|a| a == "--resume"));
        }

        // -- launch_args carry-over / extra_launch_args --------------------- //
        // PM finding, 2026-09-18 (CireSnave, via the PM, fourth AND fifth
        // real-restart retests): CireSnave launches every lane with
        // `--dangerously-load-development-channels server:claude-peers
        // --resume`. KEPT in the allowlist (a same-day retraction of an
        // earlier "deny it" fix): it's the only way non-Claude agents
        // (Synapse, FAM, claude-peers) reach a lane at all, and there is NO
        // bypass during the research preview per Claude Code's own docs.
        // It DOES show a security confirmation dialog on every start -
        // that's what `wait_for_relaunch_liveness`'s `AwaitingConfirmation`
        // outcome exists to detect, never something this allowlist itself
        // should paper over.

        #[test]
        fn ciresnaves_exact_command_line_carries_the_channels_flag_and_drops_resume() {
            let mut state = state_with_role("overmind");
            state.model = None;
            state.permission_mode = None;
            state.remote_control = false;
            state.launch_args = Some(strs(&[
                "claude.exe",
                "--dangerously-load-development-channels",
                "server:claude-peers",
                "--resume",
            ]));
            let argv = claude_argv("overmind", &state, "the prompt");
            assert_eq!(
                argv,
                strs(&[
                    "claude",
                    "the prompt",
                    "--name",
                    "overmind",
                    "--dangerously-load-development-channels",
                    "server:claude-peers",
                ]),
                "got {argv:?}"
            );
        }

        #[test]
        fn has_dev_channels_flag_detects_it_present() {
            let launch_args = strs(&[
                "claude.exe",
                "--dangerously-load-development-channels",
                "server:claude-peers",
            ]);
            assert!(has_dev_channels_flag(&launch_args));
        }

        #[test]
        fn has_dev_channels_flag_is_false_when_absent() {
            let launch_args = strs(&["claude.exe", "--model", "claude-sonnet-5"]);
            assert!(!has_dev_channels_flag(&launch_args));
        }

        #[test]
        fn a_variadic_flag_consumes_every_value_up_to_the_next_flag() {
            let launch_args = strs(&[
                "claude.exe",
                "--add-dir",
                "C:/a",
                "C:/b",
                "--model",
                "claude-sonnet-5",
            ]);
            let (extra, dropped) = extra_launch_args(&launch_args);
            assert_eq!(extra, strs(&["--add-dir", "C:/a", "C:/b"]));
            assert!(dropped.is_empty());
        }

        #[test]
        fn mcp_config_and_settings_take_exactly_one_value() {
            let launch_args = strs(&[
                "claude.exe",
                "--mcp-config",
                "C:/mcp.json",
                "--settings",
                "C:/settings.json",
            ]);
            let (extra, _) = extra_launch_args(&launch_args);
            assert_eq!(
                extra,
                strs(&[
                    "--mcp-config",
                    "C:/mcp.json",
                    "--settings",
                    "C:/settings.json"
                ])
            );
        }

        #[test]
        fn a_bare_resume_with_nothing_after_it_is_dropped_cleanly() {
            let launch_args = strs(&["claude.exe", "--resume"]);
            let (extra, dropped) = extra_launch_args(&launch_args);
            assert!(extra.is_empty());
            assert!(dropped.is_empty());
        }

        #[test]
        fn resume_followed_by_a_session_id_drops_both() {
            let launch_args = strs(&["claude.exe", "--resume", "abc-123", "--some-other-flag"]);
            let (extra, dropped) = extra_launch_args(&launch_args);
            // "abc-123" must be consumed as --resume's OWN value (dropped
            // silently with it), never left over as a stray positional or
            // misread as belonging to the next flag.
            assert!(extra.is_empty());
            assert_eq!(dropped, vec!["--some-other-flag".to_string()]);
        }

        #[test]
        fn a_positional_prompt_left_over_from_the_original_launch_is_dropped_silently() {
            let launch_args = strs(&["claude.exe", "read HANDOFF and continue"]);
            let (extra, dropped) = extra_launch_args(&launch_args);
            assert!(extra.is_empty());
            assert!(dropped.is_empty());
        }

        #[test]
        fn an_unknown_flag_is_dropped_and_reported_not_passed_through() {
            let launch_args = strs(&["claude.exe", "--some-future-flag", "value"]);
            let (extra, dropped) = extra_launch_args(&launch_args);
            // Unknown arity: only the flag itself is dropped - its
            // presumed "value" is a separate, unrecognised positional and
            // is dropped silently in its own right (same as any bare
            // positional), not folded into the flag's own report.
            assert!(extra.is_empty());
            assert_eq!(dropped, vec!["--some-future-flag".to_string()]);
        }

        #[test]
        fn claude_argv_never_duplicates_name_when_the_original_launch_already_had_one() {
            let mut state = state_with_role("overmind");
            state.launch_args = Some(strs(&["claude.exe", "--name", "old-name"]));
            let argv = claude_argv("overmind", &state, "p");
            assert_eq!(
                argv.iter().filter(|a| *a == "--name").count(),
                1,
                "got {argv:?}"
            );
            assert_eq!(argv[2], "--name");
            assert_eq!(
                argv[3], "overmind",
                "the FRESH name wins, not the stale one"
            );
        }

        #[test]
        fn claude_argv_with_no_launch_args_recorded_still_works() {
            // `state.launch_args` is None whenever the launch command line
            // couldn't be read this session - must not panic, and must not
            // invent anything.
            let mut state = state_with_role("overmind");
            state.launch_args = None;
            let argv = claude_argv("overmind", &state, "p");
            assert_eq!(
                argv,
                strs(&[
                    "claude",
                    "p",
                    "--name",
                    "overmind",
                    "--model",
                    "claude-sonnet-5",
                    "--permission-mode",
                    "prompting",
                ])
            );
        }

        #[test]
        fn no_positional_ever_directly_follows_a_variadic_flags_values() {
            // ⚠️ PM finding, 2026-09-19 (real rerun, claude's own error):
            // `--dangerously-load-development-channels` is VARIADIC and
            // consumed the trailing prompt when the prompt was appended
            // LAST - claude parsed it as an extra (invalid) channel entry
            // and exited before the dialog ever rendered. Verified against
            // the REAL claude CLI (a real spawn under portable-pty, no
            // visible window, killed within a few seconds): the old
            // (prompt-last) shape reproduces claude's exact "entries must
            // be tagged" error; prompt-first parses cleanly.
            //
            // GENERAL property, not just this one flag: walking the argv
            // from the first token after the fixed "claude <prompt> --name
            // <name>" prefix, every token must be either a recognised flag
            // or a value consumed by the immediately preceding flag's own
            // declared arity - never a stray positional a variadic flag
            // (`--dangerously-load-development-channels`, `--add-dir`)
            // could swallow.
            let known_arities: &[(&str, FlagArity)] = &[
                ("--model", FlagArity::One),
                ("--permission-mode", FlagArity::One),
                ("--remote-control", FlagArity::None),
            ];
            let mut state = state_with_role("overmind");
            state.launch_args = Some(strs(&[
                "claude.exe",
                "--dangerously-load-development-channels",
                "server:claude-peers",
                "server:extra",
                "--add-dir",
                "C:/a",
                "C:/b",
            ]));
            let argv = claude_argv("overmind", &state, "the prompt");

            assert_eq!(argv[0], "claude");
            assert_eq!(argv[1], "the prompt", "the prompt must come FIRST");
            assert_eq!(argv[2], "--name");
            let mut iter = argv[4..].iter().peekable();
            while let Some(token) = iter.next() {
                assert!(
                    is_flag_token(token),
                    "expected a flag here, found a stray positional a variadic \
                     flag could swallow: {token:?} in {argv:?}"
                );
                let arity = known_arities
                    .iter()
                    .chain(ALLOWED_LAUNCH_ARG_FLAGS.iter())
                    .find(|(f, _)| f == token)
                    .map(|(_, a)| *a)
                    .unwrap_or_else(|| panic!("unrecognised flag in built argv: {token:?}"));
                match arity {
                    FlagArity::None => {}
                    FlagArity::One => {
                        iter.next().expect("flag missing its required value");
                    }
                    FlagArity::OptionalOne => {
                        if iter.peek().is_some_and(|v| !is_flag_token(v)) {
                            iter.next();
                        }
                    }
                    FlagArity::Variadic => {
                        while iter.peek().is_some_and(|v| !is_flag_token(v)) {
                            iter.next();
                        }
                    }
                }
            }
        }

        // -- first_unsafe_argument / wt.exe ';'-rejection ----------------- //
        // PM finding, 2026-09-18: wt.exe treats ';' as ITS OWN command
        // separator - a parsing layer on top of the normal, already-safe
        // argv passing every element here goes through regardless.

        #[test]
        fn first_unsafe_argument_finds_a_semicolon_anywhere_in_the_list() {
            assert_eq!(
                first_unsafe_argument(["fine", "also fine", "not;fine"]),
                Some("not;fine")
            );
        }

        #[test]
        fn first_unsafe_argument_is_none_when_nothing_has_a_semicolon() {
            assert_eq!(first_unsafe_argument(["fine", "also fine"]), None);
        }

        #[test]
        fn kill_and_relaunch_refuses_a_cwd_containing_a_semicolon_before_touching_the_process() {
            let mut state = state_with_role("overmind");
            state.cwd = "C:/x;calc".to_string();
            let result = kill_and_relaunch(
                &NeverCalled,
                &NeverCalledStateReader,
                &state,
                &dummy_identity(),
                &mut || {},
            );
            assert!(matches!(result, Err(RelaunchError::UnsafeArgument(_))));
        }

        #[test]
        fn kill_and_relaunch_refuses_a_model_containing_a_semicolon_before_touching_the_process() {
            let mut state = state_with_role("overmind");
            state.model = Some("claude;calc".to_string());
            let result = kill_and_relaunch(
                &NeverCalled,
                &NeverCalledStateReader,
                &state,
                &dummy_identity(),
                &mut || {},
            );
            assert!(matches!(result, Err(RelaunchError::UnsafeArgument(_))));
        }

        #[test]
        fn kill_and_relaunch_refuses_a_permission_mode_containing_a_semicolon() {
            let mut state = state_with_role("overmind");
            state.permission_mode = Some("prompting;calc".to_string());
            let result = kill_and_relaunch(
                &NeverCalled,
                &NeverCalledStateReader,
                &state,
                &dummy_identity(),
                &mut || {},
            );
            assert!(matches!(result, Err(RelaunchError::UnsafeArgument(_))));
        }

        #[test]
        fn kill_and_relaunch_refuses_a_launch_arg_carried_over_flag_value_containing_a_semicolon() {
            // ⚠️ The ';' gate must cover the NEW launch_args-derived
            // elements too, not just the flags claude_argv already
            // hard-coded - it scans claude_argv's full OUTPUT, so this
            // proves the two features actually compose. `--add-dir` stands
            // in for any allowlisted flag.
            let mut state = state_with_role("overmind");
            state.launch_args = Some(strs(&["claude.exe", "--add-dir", "C:/a;calc"]));
            let result = kill_and_relaunch(
                &NeverCalled,
                &NeverCalledStateReader,
                &state,
                &dummy_identity(),
                &mut || {},
            );
            assert!(matches!(result, Err(RelaunchError::UnsafeArgument(_))));
        }

        // -- wait_for_relaunch_liveness ------------------------------------ //
        // PM finding, 2026-09-18 (fourth AND fifth real-restart retests): a
        // live process is not the same claim as a WORKING session -
        // --dangerously-load-development-channels shows a security
        // confirmation dialog a relaunched lane can sit at, alive and doing
        // nothing, until a human confirms it. Three outcomes: Relaunched
        // (real progress), AwaitingConfirmation (alive, stuck, but the
        // dev-channels flag explains why - reported ONCE, promptly, via
        // `on_awaiting`, then polling continues), and
        // Err(SessionNeverProcessedPrompt) (dead, or stuck with nothing to
        // explain it). `sleep` is faked (records calls, never actually
        // blocks) so this whole protocol - including the ~15 minute total -
        // is tested without a real wait.

        struct FakeStateReader {
            /// Returns the Nth state (0-indexed) from this list on the Nth
            /// call; the LAST entry repeats once exhausted. `None` means
            /// "no state file yet."
            states: Vec<Option<LaneState>>,
            calls: std::cell::RefCell<usize>,
        }
        impl StateReader for FakeStateReader {
            fn read(&self, _role: &str) -> Option<LaneState> {
                let mut calls = self.calls.borrow_mut();
                let idx = (*calls).min(self.states.len() - 1);
                *calls += 1;
                self.states[idx].clone()
            }
        }

        fn state_with_session_and_event(session_id: &str, event: &str) -> LaneState {
            let mut s = state_with_role("overmind");
            s.session_id = session_id.to_string();
            s.updated_by_event = event.to_string();
            s
        }

        /// Controls `find_claude_process_in`'s answer across repeated
        /// calls (the LAST entry repeats once exhausted) - every other
        /// `SystemFacts` method panics, since liveness only ever needs
        /// this one.
        struct FakeAliveFacts {
            alive_sequence: Vec<bool>,
            calls: std::cell::RefCell<usize>,
        }
        impl FakeAliveFacts {
            fn always(alive: bool) -> Self {
                Self {
                    alive_sequence: vec![alive],
                    calls: std::cell::RefCell::new(0),
                }
            }
        }
        impl SystemFacts for FakeAliveFacts {
            fn is_alive_claude_process(&self, _: u32) -> bool {
                panic!("must not be reached")
            }
            fn cwd_of(&self, _: u32) -> Option<std::path::PathBuf> {
                panic!("must not be reached")
            }
            fn has_live_shell_descendant(
                &self,
                _: u32,
            ) -> Result<bool, lane_restart::facts::ShellCheckError> {
                panic!("must not be reached")
            }
            fn transcript_is_recent(&self, _: &str, _: &str, _: std::time::Duration) -> bool {
                panic!("must not be reached")
            }
            fn now(&self) -> chrono::DateTime<chrono::Utc> {
                panic!("must not be reached")
            }
            fn process_identity(&self, _: u32) -> Option<ProcessIdentity> {
                panic!("must not be reached")
            }
            fn kill_verified(&self, _: u32, _: &ProcessIdentity) -> Result<(), KillError> {
                panic!("must not be reached")
            }
            fn find_claude_process_in(&self, _cwd: &str, _after: u64) -> Option<u32> {
                let mut calls = self.calls.borrow_mut();
                let idx = (*calls).min(self.alive_sequence.len() - 1);
                *calls += 1;
                self.alive_sequence[idx].then_some(1)
            }
        }

        #[test]
        fn liveness_succeeds_immediately_when_the_fresh_session_already_processed_a_prompt() {
            let reader = FakeStateReader {
                states: vec![Some(state_with_session_and_event(
                    "new-session",
                    "UserPromptSubmit",
                ))],
                calls: std::cell::RefCell::new(0),
            };
            let mut slept = Vec::new();
            let mut awaiting_calls = 0;
            let result = wait_for_relaunch_liveness(
                &NeverCalled,
                &reader,
                "C:/x",
                "overmind",
                "old-session",
                0,
                false,
                &mut |d| slept.push(d),
                &mut || awaiting_calls += 1,
            );
            assert_eq!(result.unwrap(), RelaunchOutcome::Relaunched);
            assert!(slept.is_empty());
            assert_eq!(awaiting_calls, 0);
        }

        #[test]
        fn liveness_polls_until_the_fresh_session_shows_progress() {
            let reader = FakeStateReader {
                states: vec![
                    None,
                    Some(state_with_session_and_event("old-session", "Stop")),
                    Some(state_with_session_and_event("new-session", "SessionStart")),
                    Some(state_with_session_and_event(
                        "new-session",
                        "UserPromptSubmit",
                    )),
                ],
                calls: std::cell::RefCell::new(0),
            };
            let mut slept = Vec::new();
            let result = wait_for_relaunch_liveness(
                &NeverCalled,
                &reader,
                "C:/x",
                "overmind",
                "old-session",
                0,
                false,
                &mut |d| slept.push(d),
                &mut || {},
            );
            assert_eq!(result.unwrap(), RelaunchOutcome::Relaunched);
            assert_eq!(slept.len(), 3);
        }

        #[test]
        fn liveness_fails_when_the_process_never_comes_up_and_nothing_explains_a_stall() {
            let reader = FakeStateReader {
                states: vec![None],
                calls: std::cell::RefCell::new(0),
            };
            let facts = FakeAliveFacts::always(false);
            let mut slept = Vec::new();
            let result = wait_for_relaunch_liveness(
                &facts,
                &reader,
                "C:/x",
                "overmind",
                "old-session",
                0,
                false,
                &mut |d| slept.push(d),
                &mut || {},
            );
            assert!(matches!(
                result,
                Err(RelaunchError::SessionNeverProcessedPrompt)
            ));
            // Gives up at the 20s progress-check, not the full ~15 minutes.
            assert_eq!(slept.len(), 20);
        }

        #[test]
        fn liveness_fails_when_the_state_file_still_shows_the_old_session_and_process_died() {
            // The old session's own Stop/SessionEnd chatter must never be
            // mistaken for the NEW session doing real work.
            let reader = FakeStateReader {
                states: vec![Some(state_with_session_and_event("old-session", "Stop"))],
                calls: std::cell::RefCell::new(0),
            };
            let facts = FakeAliveFacts::always(false);
            let result = wait_for_relaunch_liveness(
                &facts,
                &reader,
                "C:/x",
                "overmind",
                "old-session",
                0,
                false,
                &mut |_| {},
                &mut || {},
            );
            assert!(matches!(
                result,
                Err(RelaunchError::SessionNeverProcessedPrompt)
            ));
        }

        #[test]
        fn liveness_reports_awaiting_confirmation_once_when_stuck_with_the_dev_channels_flag() {
            // ⚠️ THE EXACT REAL-WORLD CASE THIS OUTCOME EXISTS TO CATCH: a
            // session stuck at the dev-channels confirmation dialog fires
            // SessionStart (a fresh session_id IS recorded) but never gets
            // past it - alive the whole time, doing nothing.
            let reader = FakeStateReader {
                states: vec![Some(state_with_session_and_event(
                    "new-session",
                    "SessionStart",
                ))],
                calls: std::cell::RefCell::new(0),
            };
            let facts = FakeAliveFacts::always(true);
            let mut awaiting_calls = 0;
            let result = wait_for_relaunch_liveness(
                &facts,
                &reader,
                "C:/x",
                "overmind",
                "old-session",
                0,
                true,
                &mut |_| {},
                &mut || awaiting_calls += 1,
            );
            assert_eq!(result.unwrap(), RelaunchOutcome::AwaitingConfirmation);
            // Reported exactly ONCE, not once per poll for the remaining
            // ~14.5 minutes - that's what makes a single prompt phone
            // notification meaningful rather than spam.
            assert_eq!(awaiting_calls, 1);
        }

        #[test]
        fn liveness_never_reports_awaiting_confirmation_without_the_dev_channels_flag() {
            // Alive and stuck is not, by itself, evidence of the dialog -
            // only report a confirmation that has real evidence behind it.
            let reader = FakeStateReader {
                states: vec![Some(state_with_session_and_event(
                    "new-session",
                    "SessionStart",
                ))],
                calls: std::cell::RefCell::new(0),
            };
            let facts = FakeAliveFacts::always(true);
            let mut awaiting_calls = 0;
            let result = wait_for_relaunch_liveness(
                &facts,
                &reader,
                "C:/x",
                "overmind",
                "old-session",
                0,
                false,
                &mut |_| {},
                &mut || awaiting_calls += 1,
            );
            assert!(matches!(
                result,
                Err(RelaunchError::SessionNeverProcessedPrompt)
            ));
            assert_eq!(awaiting_calls, 0);
        }

        #[test]
        fn liveness_flips_to_relaunched_after_a_human_confirms_the_dialog() {
            // The state file shows SessionStart for the first 25 polls (the
            // dialog is up), then progresses - a human confirmed it.
            let mut states: Vec<Option<LaneState>> = (0..25)
                .map(|_| Some(state_with_session_and_event("new-session", "SessionStart")))
                .collect();
            states.push(Some(state_with_session_and_event(
                "new-session",
                "UserPromptSubmit",
            )));
            let reader = FakeStateReader {
                states,
                calls: std::cell::RefCell::new(0),
            };
            let facts = FakeAliveFacts::always(true);
            let mut awaiting_calls = 0;
            let result = wait_for_relaunch_liveness(
                &facts,
                &reader,
                "C:/x",
                "overmind",
                "old-session",
                0,
                true,
                &mut |_| {},
                &mut || awaiting_calls += 1,
            );
            assert_eq!(result.unwrap(), RelaunchOutcome::Relaunched);
            assert_eq!(
                awaiting_calls, 1,
                "must still have notified once, at the 20s mark, even though it later resolved"
            );
        }
    }
}
