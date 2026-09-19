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
}

impl std::fmt::Display for ArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArgError::MissingRole => write!(f, "--role requires a value"),
            ArgError::RoleRequired => write!(f, "--role <name> is required"),
            ArgError::Unknown(a) => write!(f, "unrecognised argument: {a}"),
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
}

const USAGE: &str = "\
lane-restart - a bulletproof session-restart tool. RESTART-TOOL-DESIGN.md.

USAGE:
    lane-restart --role <name> [--self] [--dry-run] [--yes]
        Restart a lane. --self restarts the CALLING lane (no idle check);
        omitting it targets a DIFFERENT lane, which must be independently
        idle and requires --yes to act for real - otherwise it prints a dry
        run and does nothing.

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

    lane-restart --help
        Print this message.
";

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
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
            let model_flag = match &plan.state.model {
                Some(m) => format!(" --model {m}"),
                None => String::new(),
            };
            let permission_mode_flag = match &plan.state.permission_mode {
                Some(m) => format!(" --permission-mode {m}"),
                None => String::new(),
            };
            println!(
                "lane-restart: DRY RUN - would kill pid {} and relaunch a FRESH session via \
                 wt.exe (conhost.exe fallback): \
                 `claude --name {}{}{}{} \"read {} HANDOFF and \
                 continue\"` in {}",
                plan.state.pid,
                plan.state.name.as_deref().unwrap_or(&plan.state.role),
                model_flag,
                permission_mode_flag,
                if plan.state.remote_control {
                    " --remote-control"
                } else {
                    ""
                },
                plan.state.role,
                plan.state.cwd
            );
            log_outcome(requested_by, &args.role, "dry run - no action taken", false);
            ExitCode::SUCCESS
        }
        Ok(plan) => {
            eprintln!(
                "lane-restart: killing pid {} and relaunching a fresh session...",
                plan.state.pid
            );
            match relaunch::kill_and_relaunch(&facts, &plan.state, &plan.identity) {
                Ok(()) => {
                    log_outcome(
                        requested_by,
                        &args.role,
                        &format!(
                            "killed pid {} and relaunched a fresh session (continuity via HANDOFF only)",
                            plan.state.pid
                        ),
                        true,
                    );
                    ExitCode::SUCCESS
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
        /// PM finding, 2026-09-18: never seen a live `claude`/`claude.exe`
        /// in the target cwd within the liveness-check timeout.
        NotObservedAlive,
        /// PM finding, 2026-09-18: a `claude.exe` WAS seen in the target
        /// cwd, but was gone by the follow-up check - the graceful-exit
        /// case this whole check exists to catch (Rust's `Command`
        /// inherits the parent's std handles even under
        /// `CREATE_NEW_CONSOLE`, so a non-TTY stdin plus a prompt argument
        /// made `claude` behave like one-shot print mode: it read HANDOFF,
        /// replied, and exited).
        DiedShortlyAfterLaunch,
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
                RelaunchError::NotObservedAlive => write!(
                    f,
                    "relaunch FAILED - no live claude process was ever observed in the \
                     target directory"
                ),
                RelaunchError::DiedShortlyAfterLaunch => write!(
                    f,
                    "relaunch FAILED - a claude process came up but exited again shortly \
                     after (likely a non-interactive launch, not a real interactive session)"
                ),
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
    /// of the same fact). PM finding, 2026-09-18 (CireSnave, via the PM):
    /// CireSnave launches every lane with
    /// `--dangerously-load-development-channels server:claude-peers`, the
    /// flag that makes `claude-peers` PUSH incoming messages into a
    /// session - a relaunch that silently drops it can still SEND but
    /// never RECEIVE notifications.
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
                match arity {
                    FlagArity::None => {}
                    FlagArity::One | FlagArity::OptionalOne => {
                        if let Some(v) = iter.next() {
                            allowed.push(v.clone());
                        }
                    }
                    FlagArity::Variadic => {
                        while let Some(v) = iter.peek() {
                            if is_flag_token(v) {
                                break;
                            }
                            allowed.push(iter.next().unwrap().clone());
                        }
                    }
                }
                continue;
            }
            if let Some((_, arity)) = DROPPED_LAUNCH_ARG_FLAGS.iter().find(|(f, _)| f == token) {
                match arity {
                    FlagArity::None => {}
                    FlagArity::One => {
                        iter.next();
                    }
                    FlagArity::OptionalOne => {
                        if iter.peek().is_some_and(|v| !is_flag_token(v)) {
                            iter.next();
                        }
                    }
                    FlagArity::Variadic => {
                        while let Some(v) = iter.peek() {
                            if is_flag_token(v) {
                                break;
                            }
                            iter.next();
                        }
                    }
                }
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

    fn claude_argv(name: &str, state: &LaneState, prompt: &str) -> Vec<String> {
        let mut argv = vec!["claude".to_string(), "--name".to_string(), name.to_string()];
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
        argv.push(prompt.to_string());
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

    fn spawn_relaunch(state: &LaneState, argv: &[String]) -> Result<(), RelaunchError> {
        let mut wt = std::process::Command::new("wt.exe");
        wt.args(["-w", "new", "-d", &state.cwd]);
        wt.args(argv);
        strip_session_identity_env(&mut wt);
        match wt.spawn() {
            Ok(_) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // fall through to the conhost.exe fallback below
            }
            Err(e) => return Err(RelaunchError::Spawn(e.to_string())),
        }

        let mut conhost = std::process::Command::new("conhost.exe");
        conhost.args(argv);
        conhost.current_dir(&state.cwd);
        strip_session_identity_env(&mut conhost);
        conhost
            .spawn()
            .map(|_| ())
            .map_err(|e| RelaunchError::Spawn(e.to_string()))
    }

    /// PM finding, 2026-09-18: logging "relaunched" from `spawn()`
    /// returning `Ok` alone was dishonest - the child can start, print its
    /// reply, and exit again before anyone re-checks. This polls for a
    /// live `claude` process in `cwd` with a start_time at or after
    /// `killed_at_secs` (never an older, unrelated process), then confirms
    /// it is STILL alive a follow-up interval later. `sleep` is injected so
    /// this whole polling protocol is testable without a real ~40s wait.
    fn wait_for_relaunch_liveness(
        facts: &dyn SystemFacts,
        cwd: &str,
        killed_at_secs: u64,
        sleep: &mut dyn FnMut(std::time::Duration),
    ) -> Result<u32, RelaunchError> {
        const FIND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
        const FIND_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
        const CONFIRM_AFTER: std::time::Duration = std::time::Duration::from_secs(10);

        let mut waited = std::time::Duration::ZERO;
        let pid = loop {
            if let Some(pid) = facts.find_claude_process_in(cwd, killed_at_secs) {
                break pid;
            }
            if waited >= FIND_TIMEOUT {
                return Err(RelaunchError::NotObservedAlive);
            }
            sleep(FIND_POLL_INTERVAL);
            waited += FIND_POLL_INTERVAL;
        };

        sleep(CONFIRM_AFTER);
        if facts.is_alive_claude_process(pid) {
            Ok(pid)
        } else {
            Err(RelaunchError::DiedShortlyAfterLaunch)
        }
    }

    pub fn kill_and_relaunch(
        facts: &dyn SystemFacts,
        state: &LaneState,
        identity: &ProcessIdentity,
    ) -> Result<(), RelaunchError> {
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
        // without either being "wrong". Without slack, a genuinely fresh
        // relaunch could be excluded as "too old" by a rounding difference
        // between the two - confirmed live: this crate's own real-process
        // liveness test failed on a CI runner for exactly this reason
        // before the margin was added. §2's real identity check (pid, cwd,
        // session transcript, exe) still does the actual verification;
        // this threshold only needs to rule out a stale, unrelated process
        // from BEFORE the kill, not pin the exact second.
        let killed_at_secs = facts.now().timestamp().max(0).saturating_sub(5) as u64;

        spawn_relaunch(state, &argv)?;

        wait_for_relaunch_liveness(facts, &state.cwd, killed_at_secs, &mut std::thread::sleep)
            .map(|_pid| ())
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
            let result = kill_and_relaunch(&NeverCalled, &state, &dummy_identity());
            assert!(matches!(result, Err(RelaunchError::InvalidIdentifier(_))));
        }

        #[test]
        fn kill_and_relaunch_refuses_an_invalid_name_before_touching_the_process() {
            let mut state = state_with_role("overmind");
            state.name = Some("a|calc".to_string());
            let result = kill_and_relaunch(&NeverCalled, &state, &dummy_identity());
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

        // -- claude_argv ------------------------------------------------- //

        #[test]
        fn claude_argv_omits_model_and_permission_mode_when_absent() {
            let mut state = state_with_role("overmind");
            state.model = None;
            state.permission_mode = None;
            let argv = claude_argv("overmind", &state, "the prompt");
            assert_eq!(
                argv,
                vec!["claude", "--name", "overmind", "the prompt"]
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
        // PM finding, 2026-09-18 (CireSnave, via the PM): CireSnave launches
        // every lane with `--dangerously-load-development-channels
        // server:claude-peers --resume` - a relaunch that only rebuilds
        // --model/--permission-mode/--remote-control silently drops the
        // channels flag, so a relaunched lane can send but never RECEIVE
        // claude-peers notifications.

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
                    "--name",
                    "overmind",
                    "--dangerously-load-development-channels",
                    "server:claude-peers",
                    "the prompt",
                ]),
                "got {argv:?}"
            );
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
            assert_eq!(argv[1], "--name");
            assert_eq!(
                argv[2], "overmind",
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
                    "--name",
                    "overmind",
                    "--model",
                    "claude-sonnet-5",
                    "--permission-mode",
                    "prompting",
                    "p"
                ])
            );
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
            let result = kill_and_relaunch(&NeverCalled, &state, &dummy_identity());
            assert!(matches!(result, Err(RelaunchError::UnsafeArgument(_))));
        }

        #[test]
        fn kill_and_relaunch_refuses_a_model_containing_a_semicolon_before_touching_the_process() {
            let mut state = state_with_role("overmind");
            state.model = Some("claude;calc".to_string());
            let result = kill_and_relaunch(&NeverCalled, &state, &dummy_identity());
            assert!(matches!(result, Err(RelaunchError::UnsafeArgument(_))));
        }

        #[test]
        fn kill_and_relaunch_refuses_a_permission_mode_containing_a_semicolon() {
            let mut state = state_with_role("overmind");
            state.permission_mode = Some("prompting;calc".to_string());
            let result = kill_and_relaunch(&NeverCalled, &state, &dummy_identity());
            assert!(matches!(result, Err(RelaunchError::UnsafeArgument(_))));
        }

        #[test]
        fn kill_and_relaunch_refuses_a_launch_arg_carried_over_flag_value_containing_a_semicolon() {
            // ⚠️ The ';' gate must cover the NEW launch_args-derived
            // elements too, not just the flags claude_argv already
            // hard-coded - it scans claude_argv's full OUTPUT, so this
            // proves the two features actually compose.
            let mut state = state_with_role("overmind");
            state.launch_args = Some(strs(&[
                "claude.exe",
                "--dangerously-load-development-channels",
                "server:claude-peers;calc",
            ]));
            let result = kill_and_relaunch(&NeverCalled, &state, &dummy_identity());
            assert!(matches!(result, Err(RelaunchError::UnsafeArgument(_))));
        }

        // -- wait_for_relaunch_liveness ------------------------------------ //
        // PM finding, 2026-09-18: logging "relaunched" from spawn() = Ok
        // alone was dishonest - the child can start, reply, and exit again
        // before anyone re-checks. `sleep` is faked here (records calls,
        // never actually blocks) so this whole polling protocol is tested
        // without a real ~40s wait.

        struct LivenessFacts {
            /// Returns `Some(pid)` starting from the Nth call (0-indexed);
            /// `None` on every call before that. `usize::MAX` = never found.
            found_on_call: usize,
            pid: u32,
            /// Whether `is_alive_claude_process(pid)` answers true at the
            /// follow-up check.
            still_alive_at_confirm: bool,
            find_calls: std::cell::RefCell<usize>,
        }
        impl SystemFacts for LivenessFacts {
            fn is_alive_claude_process(&self, pid: u32) -> bool {
                pid == self.pid && self.still_alive_at_confirm
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
                let mut calls = self.find_calls.borrow_mut();
                let this_call = *calls;
                *calls += 1;
                if this_call >= self.found_on_call {
                    Some(self.pid)
                } else {
                    None
                }
            }
        }

        #[test]
        fn liveness_succeeds_when_found_immediately_and_still_alive_at_confirm() {
            let facts = LivenessFacts {
                found_on_call: 0,
                pid: 42,
                still_alive_at_confirm: true,
                find_calls: std::cell::RefCell::new(0),
            };
            let mut slept = Vec::new();
            let result = wait_for_relaunch_liveness(&facts, "C:/x", 0, &mut |d| slept.push(d));
            assert_eq!(result.unwrap(), 42);
            // Only the confirm-interval sleep, no find-polling sleep needed.
            assert_eq!(slept.len(), 1);
        }

        #[test]
        fn liveness_polls_until_found_then_confirms() {
            let facts = LivenessFacts {
                found_on_call: 3,
                pid: 7,
                still_alive_at_confirm: true,
                find_calls: std::cell::RefCell::new(0),
            };
            let mut slept = Vec::new();
            let result = wait_for_relaunch_liveness(&facts, "C:/x", 0, &mut |d| slept.push(d));
            assert_eq!(result.unwrap(), 7);
            // 3 find-poll sleeps (calls 0,1,2 returned None) + 1 confirm sleep.
            assert_eq!(slept.len(), 4);
        }

        #[test]
        fn liveness_fails_not_observed_alive_when_never_found_within_the_timeout() {
            let facts = LivenessFacts {
                found_on_call: usize::MAX,
                pid: 1,
                still_alive_at_confirm: true,
                find_calls: std::cell::RefCell::new(0),
            };
            let mut slept = Vec::new();
            let result = wait_for_relaunch_liveness(&facts, "C:/x", 0, &mut |d| slept.push(d));
            assert!(matches!(result, Err(RelaunchError::NotObservedAlive)));
            // Never sleeps the confirm interval - it gave up first.
            assert!(!slept.contains(&std::time::Duration::from_secs(10)));
        }

        #[test]
        fn liveness_fails_died_shortly_after_launch_when_gone_at_the_confirm_check() {
            // ⚠️ THE EXACT REAL-WORLD FAILURE THIS CHECK EXISTS TO CATCH:
            // found alive once, gone by the follow-up - a graceful exit,
            // not a kill (the PM's real retest: replied, then exited 12s
            // later with a clean SessionEnd).
            let facts = LivenessFacts {
                found_on_call: 0,
                pid: 42,
                still_alive_at_confirm: false,
                find_calls: std::cell::RefCell::new(0),
            };
            let mut slept = Vec::new();
            let result = wait_for_relaunch_liveness(&facts, "C:/x", 0, &mut |d| slept.push(d));
            assert!(matches!(result, Err(RelaunchError::DiedShortlyAfterLaunch)));
        }
    }
}
