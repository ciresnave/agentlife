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
                "lane-restart: DRY RUN - would kill pid {} and relaunch a FRESH session: \
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
        Kill(KillError),
        Spawn(String),
    }

    impl std::fmt::Display for RelaunchError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                RelaunchError::InvalidIdentifier(what) => {
                    write!(f, "refusing to launch - not a plain identifier: {what}")
                }
                RelaunchError::Kill(e) => write!(f, "kill refused: {e}"),
                RelaunchError::Spawn(e) => write!(f, "could not launch the relaunch command: {e}"),
            }
        }
    }

    /// ⚠️ NEVER `--resume`. PM finding, 2026-09-18: resuming reloads the
    /// whole prior transcript, carrying the full context back in - exactly
    /// the per-turn cost a restart exists to cut. This launches a FRESH
    /// session; the only continuity is the lane's own HANDOFF file, read
    /// by the first prompt. `state.session_id` is used only by
    /// `authorize::decide`'s identity check (RESTART-TOOL-DESIGN.md §2),
    /// never here.
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

        facts
            .kill_verified(state.pid, identity)
            .map_err(RelaunchError::Kill)?;
        // Give the OS a moment to finish tearing the process down before a
        // new `claude` process claims the same working directory's lock.
        std::thread::sleep(std::time::Duration::from_millis(500));

        let prompt = format!("read {} HANDOFF and continue", state.role);
        let mut cmd = std::process::Command::new("claude");
        cmd.args(["--name", name]);
        if let Some(model) = &state.model {
            cmd.args(["--model", model]);
        }
        if let Some(permission_mode) = &state.permission_mode {
            cmd.args(["--permission-mode", permission_mode]);
        }
        if state.remote_control {
            cmd.arg("--remote-control");
        }
        cmd.arg(&prompt);
        cmd.current_dir(&state.cwd);

        // ⚠️ NO SHELL. `claude.exe` is spawned directly with a real argv
        // array; `CREATE_NEW_CONSOLE` gives it the visible window
        // RESTART-TOOL-DESIGN.md §5 asks for without cmd.exe ever parsing
        // anything a lane wrote about itself.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
            cmd.creation_flags(CREATE_NEW_CONSOLE);
        }

        cmd.spawn()
            .map_err(|e| RelaunchError::Spawn(e.to_string()))?;
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

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
    }
}
