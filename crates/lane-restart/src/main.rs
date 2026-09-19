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
    fn spawn_relaunch(state: &LaneState, argv: &[String]) -> Result<(), RelaunchError> {
        let mut wt = std::process::Command::new("wt.exe");
        wt.args(["-w", "new", "-d", &state.cwd]);
        wt.args(argv);
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
