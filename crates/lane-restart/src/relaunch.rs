// SPDX-License-Identifier: MIT OR Apache-2.0

//! The only place a real kill signal or a real process spawn happens.
//! Deliberately NOT covered by `authorize.rs`'s tests (which never touch a
//! real process) - `valid_identifier` and the argv SHAPE are unit tested
//! here; the actual OS calls are exercised by `--dry-run` against real
//! lanes before `--yes` is ever used for real.

use crate::facts::{KillError, ProcessIdentity, SystemFacts};
use crate::launch::{
    launch_argv, prepare_launch, wait_for_liveness, LaunchSpec, LivenessTiming, PreparedLaunch,
    SpawnFn,
};
use crate::spec::{self, DirSpecStore, NoSpecStore, SpecState, SpecStore};
use crate::state::LaneState;
use crate::stop::stop_lane;

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
    lane_state::role::is_valid(s)
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
    /// CireSnave, 2026-10-08: "I can't afford Opus." The resolved model
    /// id names Opus and no override was given.
    /// A model that is not Sonnet or Haiku, without an [`OpusPermit`].
    ModelRefused(String),
    /// RESTART-TOOL-DESIGN.md 14: the launch spec could not be written, complete and read back,
    /// so the lane was NOT stopped - a lane may be stopped only if it can be started again.
    SpecNotWritten(String),
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
            RelaunchError::SpecNotWritten(why) => write!(
                f,
                "refusing to stop - the launch spec could not be written, so the lane could \
                 not be started again: {why}"
            ),
            RelaunchError::ModelRefused(model) => write!(
                f,
                "refusing to relaunch on model {model:?}: only sonnet and haiku models run without {ALLOW_OPUS_ENV}=1 (an Opus or unknown name is refused, never guessed)"
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
    ("-n", FlagArity::One),
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
pub fn extra_launch_args(launch_args: &[String]) -> (Vec<String>, Vec<String>) {
    let mut allowed = Vec::new();
    let mut dropped_unknown = Vec::new();
    let mut iter = launch_args.iter().skip(1).peekable();
    while let Some(token) = iter.next() {
        // `--permission-mode=value` is already carried over via
        // state.permission_mode (see DROPPED_LAUNCH_ARG_FLAGS's own
        // comment) - the equals form embeds its value in the same
        // token, so it never matches an exact `--permission-mode`
        // lookup below and would otherwise be misread as unknown.
        // The same for `--name=value`: the relaunch emits a fresh `--name`.
        if token.starts_with("--permission-mode=") || token.starts_with("--name=") {
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
pub fn has_dev_channels_flag(launch_args: &[String]) -> bool {
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
pub fn claude_argv(name: &str, state: &LaneState, prompt: &str, model: &str) -> Vec<String> {
    let spec = LaunchSpec {
        name: name.to_string(),
        ..LaunchSpec::from(state)
    };
    launch_argv(&spec, prompt, model)
}

/// PM finding, 2026-09-18: `wt.exe`'s own argument parser reads `;` as
/// a command separator, independent of and in addition to the normal
/// (already-safe) argv passing every element here goes through. Every
/// element that reaches `wt.exe` - including `cwd`, which is passed as
/// its own `-d` argument - must be checked, not just the ones this
/// module builds itself.
pub fn first_unsafe_argument<'a>(elements: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    elements.into_iter().find(|e| e.contains(';'))
}

/// The command line a launch hands to `wt.exe` / `conhost.exe`, checked as a whole: the working
/// directory and every element of the host-wrapped argv, including the `lane-restart.exe` path
/// that [`host_wrapped_argv`] prepends. It can only be built by [`HostedArgv::new`], which refuses
/// a `;` anywhere in it ([`first_unsafe_argument`]), and [`spawn_launch`] accepts nothing else, so
/// an unchecked command line cannot be spawned (audit group 5, finding 2). It derefs to the CLAUDE
/// argv (what the lane runs), which is what callers display and compare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedArgv {
    cwd: String,
    claude: Vec<String>,
    hosted: Vec<String>,
}

impl HostedArgv {
    pub fn new(cwd: &str, role: &str, claude_argv: Vec<String>) -> Result<Self, RelaunchError> {
        let hosted = host_wrapped_argv(role, &claude_argv);
        if let Some(bad) =
            first_unsafe_argument(std::iter::once(cwd).chain(hosted.iter().map(String::as_str)))
        {
            return Err(RelaunchError::UnsafeArgument(bad.to_string()));
        }
        Ok(Self {
            cwd: cwd.to_string(),
            claude: claude_argv,
            hosted,
        })
    }

    /// The working directory the checked command line starts in.
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    /// `lane-restart host --role <role> -- <claude argv>`, exactly as it is spawned.
    pub fn hosted(&self) -> &[String] {
        &self.hosted
    }
}

impl std::ops::Deref for HostedArgv {
    type Target = [String];

    fn deref(&self) -> &[String] {
        &self.claude
    }
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
use lane_state::claude_proc::SESSION_IDENTITY_ENV_VARS;

pub fn strip_session_identity_env(cmd: &mut std::process::Command) {
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
pub fn host_wrapped_argv(role: &str, claude_argv: &[String]) -> Vec<String> {
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

/// Env var that lets a relaunch use an Opus model (CireSnave's
/// 2026-10-08 rule: never, unless explicitly overridden).
pub const ALLOW_OPUS_ENV: &str = "LANE_RESTART_ALLOW_OPUS";

/// The portfolio default model: `{"model": "..."}` in
/// `model-policy.json` under `state_dir`, else `sonnet`. A missing or
/// unreadable file yields `sonnet`, never the session's own model.
pub fn policy_default_model(state_dir: &std::path::Path) -> String {
    std::fs::read(state_dir.join("model-policy.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v.get("model")?.as_str().map(str::to_string))
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| "sonnet".to_string())
}

/// Permission to run an Opus (or any non-Sonnet, non-Haiku) model. Holding one is the only way
/// [`Model::parse`] accepts such a name, and the only constructor is [`OpusPermit::from_env`], which
/// is crate-private and called only by the interactive relaunch/launch entry points: the launcher
/// service, `lane-start`, spec import and the stop path have no way to obtain one, so "Opus without
/// permission" is a compile error there, not a `bool` somebody forgot to pass (audit group 5,
/// finding 3). The permit is still an honest human gate, not a security boundary: any process can set
/// the environment variable.
#[derive(Debug, Clone, Copy)]
pub struct OpusPermit(());

impl OpusPermit {
    /// `Some` only when [`ALLOW_OPUS_ENV`] is exactly `1` in this process's environment.
    pub(crate) fn from_env() -> Option<Self> {
        std::env::var(ALLOW_OPUS_ENV)
            .is_ok_and(|v| v == "1")
            .then_some(Self(()))
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self(())
    }
}

/// Whether `name` is a model that needs no permit: `sonnet`, `haiku`, `claude-sonnet-*` or
/// `claude-haiku-*` (an optional `[...]` context suffix is ignored), and not an Opus name by any
/// spelling. FAIL CLOSED: an unknown or future name is not on this list (CireSnave, 2026-10-08:
/// "I can't afford Opus"; pin Sonnet or Haiku explicitly).
pub fn is_permitted_model(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    let base = lower.split('[').next().unwrap_or("");
    !lower.contains("opus")
        && (base == "sonnet"
            || base == "haiku"
            || base.starts_with("claude-sonnet-")
            || base.starts_with("claude-haiku-"))
}

/// A model name that [`Model::parse`] accepted. Holds a name only if it is permitted or the caller
/// presented an [`OpusPermit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model(String);

impl Model {
    pub fn parse(name: &str, permit: Option<&OpusPermit>) -> Result<Self, RelaunchError> {
        if is_permitted_model(name) || permit.is_some() {
            Ok(Self(name.to_string()))
        } else {
            Err(RelaunchError::ModelRefused(name.to_string()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for Model {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl PartialEq<&str> for Model {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl std::fmt::Display for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub fn is_opus(model: &str) -> bool {
    model.to_ascii_lowercase().contains("opus")
}

/// The `--model` value of the ORIGINAL launch, if it had one: the only
/// explicit per-lane setting. `state.model` is deliberately NOT used -
/// it records what the session happened to run on, not what was chosen.
fn explicit_launch_model(launch_args: Option<&Vec<String>>) -> Option<String> {
    let args = launch_args?;
    let i = args.iter().position(|a| a == "--model")?;
    args.get(i + 1).filter(|v| !is_flag_token(v)).cloned()
}

/// Model a relaunch is pinned to: explicit launch `--model`, else
/// `policy_default`. Anything but Sonnet or Haiku is refused without a permit.
pub fn resolve_model(
    state: &LaneState,
    policy_default: &str,
    permit: Option<&OpusPermit>,
) -> Result<Model, RelaunchError> {
    resolve_launch_model(state.launch_args.as_ref(), policy_default, permit)
}

/// `resolve_model`, from the launch flags alone (a `LaunchSpec` has no state).
pub fn resolve_launch_model(
    launch_args: Option<&Vec<String>>,
    policy_default: &str,
    permit: Option<&OpusPermit>,
) -> Result<Model, RelaunchError> {
    Model::parse(&recorded_launch_model(launch_args, policy_default), permit)
}

/// The model name a launch's flags (else the policy default) name, as plain text, UNCHECKED. Only
/// for reading a lane that is already running (the stop path records and replaces it); nothing
/// launches from this string - a launch needs a [`Model`].
pub fn recorded_launch_model(launch_args: Option<&Vec<String>>, policy_default: &str) -> String {
    explicit_launch_model(launch_args).unwrap_or_else(|| policy_default.to_string())
}

/// Read-only: one line per lane state file, its recorded model, Opus flagged.
pub fn models_report(state_dir: &std::path::Path) -> String {
    let mut lines = Vec::new();
    if let Ok(entries) = std::fs::read_dir(state_dir) {
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(spec::is_spec_file_name)
            {
                continue;
            }
            let Some(role) = path.file_stem().and_then(|x| x.to_str()) else {
                continue;
            };
            let Ok(st) = crate::state::load(state_dir, role) else {
                continue;
            };
            let model = st.model.as_deref().unwrap_or("-");
            let flag = if is_opus(model) { "  <-- OPUS" } else { "" };
            lines.push(format!("{role}\t{model}{flag}"));
        }
    }
    lines.sort();
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// The child's role for its own `lane_state_writer::resolve_role`, where `LANE_ROLE` always wins.
/// Set explicitly on every launch: a peer's launch (the PM running `--role x`) would otherwise hand
/// the child the PM's own `LANE_ROLE`, and the child would write the PM's role file, not its own.
fn set_lane_role(cmd: &mut std::process::Command, role: &str) {
    cmd.env("LANE_ROLE", role);
}

/// The `wt.exe` command and the `conhost.exe` fallback for one launch, not yet started. Windows
/// only in production; compiled everywhere so its tests run on both legs.
#[cfg_attr(not(windows), allow(dead_code))]
fn launch_commands(
    spec: &LaunchSpec,
    argv: &HostedArgv,
) -> (std::process::Command, std::process::Command) {
    let hosted_argv = argv.hosted();
    let mut wt = std::process::Command::new("wt.exe");
    wt.args(["-w", "new", "-d", argv.cwd()]);
    wt.args(hosted_argv);
    strip_session_identity_env(&mut wt);
    set_lane_role(&mut wt, &spec.role);

    let mut conhost = std::process::Command::new("conhost.exe");
    conhost.args(hosted_argv);
    conhost.current_dir(&spec.cwd);
    strip_session_identity_env(&mut conhost);
    set_lane_role(&mut conhost, &spec.role);
    (wt, conhost)
}

/// The direct launch (`docs/PORT-LINUX-PLAN.md`, P6; ruled by CireSnave 2026-10-14: *"Direct process
/// first"*): the host itself, started detached with no terminal. No `wt.exe`, no terminal emulator,
/// no tmux. Its stdin is the null device and its output goes to `host-<role>.out` in the state
/// directory (the null device if that cannot be opened); the host makes itself a session leader
/// when it finds it has no tty (`host::detach_session_if_headless`).
///
/// Pure: nothing is spawned here, so the command is testable without a process.
pub fn direct_command(
    spec: &LaunchSpec,
    hosted_argv: &[String],
    out_file: Option<std::fs::File>,
) -> std::process::Command {
    let (program, args) = hosted_argv
        .split_first()
        .map(|(p, a)| (p.as_str(), a))
        .unwrap_or(("lane-restart", &[]));
    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    cmd.current_dir(&spec.cwd);
    strip_session_identity_env(&mut cmd);
    set_lane_role(&mut cmd, &spec.role);
    cmd.stdin(std::process::Stdio::null());
    match out_file.and_then(|f| f.try_clone().ok().map(|g| (f, g))) {
        Some((out, err)) => {
            cmd.stdout(out).stderr(err);
        }
        None => {
            cmd.stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
        }
    }
    cmd
}

/// Starts [`direct_command`]. The child is not waited for: the caller watches for the new session
/// the same way it does after a `wt.exe` launch.
fn spawn_direct(spec: &LaunchSpec, argv: &HostedArgv) -> Result<(), RelaunchError> {
    spawn_direct_argv(spec, argv.hosted(), &crate::state_dir())
}

fn spawn_direct_argv(
    spec: &LaunchSpec,
    hosted_argv: &[String],
    out_dir: &std::path::Path,
) -> Result<(), RelaunchError> {
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(out_dir.join(format!("host-{}.out", spec.role)))
        .ok();
    direct_command(spec, hosted_argv, out)
        .spawn()
        .map(|_| ())
        .map_err(|e| RelaunchError::Spawn(e.to_string()))
}

pub fn spawn_launch(spec: &LaunchSpec, argv: &HostedArgv) -> Result<(), RelaunchError> {
    // Off Windows there is no `wt.exe`/`conhost.exe`: the lane host runs directly.
    if !cfg!(windows) {
        return spawn_direct(spec, argv);
    }
    let (mut wt, mut conhost) = launch_commands(spec, argv);
    match wt.spawn() {
        Ok(_) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // fall through to the conhost.exe fallback below
        }
        Err(e) => return Err(RelaunchError::Spawn(e.to_string())),
    }

    conhost
        .spawn()
        .map(|_| ())
        .map_err(|e| RelaunchError::Spawn(e.to_string()))
}

pub fn spawn_relaunch(state: &LaneState, argv: &HostedArgv) -> Result<(), RelaunchError> {
    spawn_launch(&LaunchSpec::from(state), argv)
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
        crate::state::load(self.state_dir, role).ok()
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
pub fn wait_for_relaunch_liveness(
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
    crate::launch::wait_for_liveness(
        facts,
        state_reader,
        cwd,
        role,
        Some(old_session_id),
        killed_at_secs,
        carries_dev_channels_flag,
        &LivenessTiming::default(),
        sleep,
        on_awaiting,
    )
}

/// PM finding, 2026-09-19: the dry-run message used to rebuild its own
/// string by hand, separately from the argv the real launch actually
/// uses - and silently dropped the carried-over `launch_args` flags
/// (`--dangerously-load-development-channels` included) doing so. This
/// calls the EXACT SAME `claude_argv` the real launch calls, so the two
/// can never drift apart again.
pub fn describe_dry_run(state: &LaneState) -> Result<Vec<String>, RelaunchError> {
    Ok(prepare(&LaunchSpec::from(state))?.argv.to_vec())
}

/// `prepare_launch` with this machine's policy default model and the Opus
/// override from the environment.
fn prepare(spec: &LaunchSpec) -> Result<PreparedLaunch, RelaunchError> {
    prepare_launch(
        spec,
        &policy_default_model(&crate::state_dir()),
        OpusPermit::from_env().as_ref(),
    )
}

/// `kill_and_relaunch_persisting` without a spec store: persists nothing. For callers that
/// inject their own spawn (tests, a batch restore); the real restart is `kill_and_relaunch`,
/// which persists.
#[allow(clippy::too_many_arguments)]
pub fn kill_and_relaunch_with(
    facts: &dyn SystemFacts,
    state_reader: &dyn StateReader,
    state: &LaneState,
    identity: &ProcessIdentity,
    policy_default: &str,
    permit: Option<&OpusPermit>,
    spawn: &SpawnFn,
    sleep: &mut dyn FnMut(std::time::Duration),
    on_awaiting: &mut dyn FnMut(),
) -> Result<RelaunchOutcome, RelaunchError> {
    kill_and_relaunch_persisting(
        facts,
        state_reader,
        state,
        identity,
        policy_default,
        permit,
        &NoSpecStore,
        spawn,
        sleep,
        on_awaiting,
    )
}

/// `kill_and_relaunch` with its spawn, sleep, policy and spec store injected so the ORDER is
/// testable: every check, then **the launch spec written, complete and read back**
/// (RESTART-TOOL-DESIGN.md 14.3), then the stop (`stop::stop_lane`), then the start
/// (`launch::wait_for_liveness` after the spawn), then the spec marked `running`. A restart is
/// `lane-stop` then `lane-start` (spec 8b) plus what only a restart needs.
///
/// The spec write is before the kill and its failure refuses the restart: that ordering IS the
/// invariant (a lane may be stopped only if it can be started again).
#[allow(clippy::too_many_arguments)]
pub fn kill_and_relaunch_persisting(
    facts: &dyn SystemFacts,
    state_reader: &dyn StateReader,
    state: &LaneState,
    identity: &ProcessIdentity,
    policy_default: &str,
    permit: Option<&OpusPermit>,
    specs: &dyn SpecStore,
    spawn: &SpawnFn,
    sleep: &mut dyn FnMut(std::time::Duration),
    on_awaiting: &mut dyn FnMut(),
) -> Result<RelaunchOutcome, RelaunchError> {
    let spec = LaunchSpec::from(state);
    // every check runs BEFORE anything is killed
    // Checked here, before anything is written or killed; the launch itself comes from the
    // spec below.
    prepare_launch(&spec, policy_default, permit)?;
    let model = resolve_launch_model(spec.launch_args.as_ref(), policy_default, permit)?;
    let spec_inputs = |spec_state: SpecState, stopped: bool| spec::SpecInputs {
        launch: &spec,
        model: model.as_str(),
        session_id: stopped.then_some(state.session_id.as_str()),
        pid_start_secs: stopped.then_some(identity.start_time_secs),
        state: spec_state,
        written_by: "restart",
        now: chrono::Utc::now(),
    };
    let stopping = spec::build(&spec_inputs(SpecState::Stopping, true))
        .map_err(|e| RelaunchError::SpecNotWritten(e.to_string()))?;
    specs
        .write(&stopping)
        .map_err(|e| RelaunchError::SpecNotWritten(e.to_string()))?;
    // RESTART-TOOL-DESIGN.md 14.7: the new session starts from THE SPEC JUST WRITTEN (and read
    // back by the store), not from the state file it was derived from, so a `lane-stop` then a
    // `lane-start` and this restart launch the same thing. Prepared BEFORE the kill: a spec that
    // would not launch must not cost a running lane.
    let from_spec = stopping.launch_spec();
    let PreparedLaunch {
        argv,
        carries_dev_channels_flag,
    } = prepare_launch(&from_spec, &stopping.model, None)?;
    let stopped = stop_lane(facts, state.pid, identity, sleep)?;
    spawn(&from_spec, &argv)?;
    let outcome = wait_for_liveness(
        facts,
        state_reader,
        &state.cwd,
        &state.role,
        Some(&state.session_id),
        stopped.launched_after_secs,
        carries_dev_channels_flag,
        &LivenessTiming::default(),
        sleep,
        on_awaiting,
    )?;
    // The lane is up with these parameters. Best effort: a failure here leaves the `stopping`
    // spec, which is complete and reads as `crashed` if the new session later ends.
    let running = spec::build(&spec_inputs(SpecState::Running, false))
        .map_err(|e| e.to_string())
        .and_then(|s| specs.write(&s).map_err(|e| e.to_string()));
    if let Err(why) = running {
        eprintln!(
            "lane-restart: the lane is up, but its launch spec could not be marked running: {why}"
        );
    }
    Ok(outcome)
}

pub fn kill_and_relaunch(
    facts: &dyn SystemFacts,
    state_reader: &dyn StateReader,
    state: &LaneState,
    identity: &ProcessIdentity,
    on_awaiting: &mut dyn FnMut(),
) -> Result<RelaunchOutcome, RelaunchError> {
    kill_and_relaunch_persisting(
        facts,
        state_reader,
        state,
        identity,
        &policy_default_model(&crate::state_dir()),
        OpusPermit::from_env().as_ref(),
        &DirSpecStore {
            dir: crate::state_dir(),
        },
        &spawn_launch,
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
        fn has_live_shell_descendant(&self, _: u32) -> Result<bool, crate::facts::ShellCheckError> {
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
        fn process_table(
            &self,
        ) -> Result<Vec<crate::facts::ProcEntry>, crate::facts::ShellCheckError> {
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
            pid_start_secs: None,
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

    // -- LANE_ROLE on a launch ----------------------------------------- //
    // `LANE_ROLE`, when set, always wins in `resolve_role`. A launch run by a
    // peer inherited the peer's, so the child wrote the peer's role file.

    fn direct_spec() -> LaunchSpec {
        LaunchSpec {
            role: "baracuda".to_string(),
            name: "baracuda".to_string(),
            cwd: "C:/Projects/baracuda".to_string(),
            permission_mode: None,
            remote_control: false,
            launch_args: None,
        }
    }

    /// The direct launch is the host itself: no terminal program in front of it, the lane's own
    /// directory and role, and no inherited session identity.
    #[test]
    fn the_direct_command_is_the_host_in_the_lanes_directory_with_its_own_role() {
        let hosted = strs(&[
            "/opt/lane-restart",
            "host",
            "--role",
            "baracuda",
            "--",
            "claude",
            "go",
        ]);
        let cmd = direct_command(&direct_spec(), &hosted, None);
        assert_eq!(cmd.get_program(), "/opt/lane-restart");
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["host", "--role", "baracuda", "--", "claude", "go"]);
        assert_eq!(
            cmd.get_current_dir().unwrap().to_string_lossy(),
            "C:/Projects/baracuda"
        );
        let role = cmd
            .get_envs()
            .find(|(k, _)| *k == "LANE_ROLE")
            .and_then(|(_, v)| v)
            .map(|v| v.to_string_lossy().into_owned());
        assert_eq!(role.as_deref(), Some("baracuda"));
        // no wt.exe/conhost.exe anywhere in it
        let all = format!("{:?}", cmd);
        assert!(!all.contains("wt.exe") && !all.contains("conhost"), "{all}");
    }

    /// Real spawn, unix only: the direct launch runs the command it is given, detached from our
    /// stdio, in the spec's directory.
    #[cfg(unix)]
    #[test]
    fn a_direct_launch_runs_its_command_in_the_specs_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = direct_spec();
        spec.cwd = dir.path().to_string_lossy().into_owned();
        let argv = strs(&[
            "sh",
            "-c",
            "pwd > where.txt; echo \"$LANE_ROLE\" > role.txt",
        ]);
        spawn_direct_argv(&spec, &argv, dir.path()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let role_file = dir.path().join("role.txt");
        while !role_file.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert_eq!(
            std::fs::read_to_string(&role_file).unwrap().trim(),
            "baracuda"
        );
        let pwd = std::fs::read_to_string(dir.path().join("where.txt")).unwrap();
        assert_eq!(
            std::fs::canonicalize(pwd.trim()).unwrap(),
            std::fs::canonicalize(dir.path()).unwrap()
        );
    }

    #[test]
    fn both_launch_commands_set_the_launched_role_not_the_spawners() {
        let spec = LaunchSpec {
            role: "overmind".to_string(),
            cwd: "C:/Projects/OverMind".to_string(),
            ..LaunchSpec::from(&state_with_role("overmind"))
        };
        // An explicit set overrides whatever the spawner's own environment carries.
        let argv = HostedArgv::new(&spec.cwd, &spec.role, strs(&["claude"])).unwrap();
        let (wt, conhost) = launch_commands(&spec, &argv);
        for (what, cmd) in [("wt", &wt), ("conhost", &conhost)] {
            let envs: std::collections::HashMap<_, _> = cmd.get_envs().collect();
            assert_eq!(
                envs.get(std::ffi::OsStr::new("LANE_ROLE")),
                Some(&Some(std::ffi::OsStr::new("overmind"))),
                "{what} must set LANE_ROLE to the launched role"
            );
        }
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

    // -- model policy (PM TASK 2026-10-08) --------------------------- //
    // CireSnave, 2026-10-08: "I can't afford Opus." The model a session
    // happened to be RUNNING (state.model) must never decide the
    // relaunch's model: only an explicit `--model` in the original
    // launch, else the portfolio policy default.

    #[test]
    fn a_state_recording_opus_relaunches_on_the_policy_default_not_opus() {
        let mut state = state_with_role("overmind");
        state.model = Some("claude-opus-5-5".to_string());
        state.launch_args = None;
        let model = resolve_model(&state, "sonnet", None).unwrap();
        assert_eq!(model, "sonnet");
        let argv = claude_argv("overmind", &state, "p", &model);
        assert!(!argv.iter().any(|a| a.contains("opus")), "got {argv:?}");
        let i = argv.iter().position(|a| a == "--model").unwrap();
        assert_eq!(argv[i + 1], "sonnet");
    }

    #[test]
    fn an_explicit_launch_model_pin_wins_over_the_policy_default() {
        let mut state = state_with_role("overmind");
        state.model = Some("claude-opus-5-5".to_string());
        state.launch_args = Some(strs(&["claude.exe", "--model", "haiku"]));
        assert_eq!(resolve_model(&state, "sonnet", None).unwrap(), "haiku");
    }

    #[test]
    fn an_explicit_opus_pin_is_refused_without_the_override() {
        let mut state = state_with_role("overmind");
        state.launch_args = Some(strs(&["claude.exe", "--model", "opus"]));
        assert!(matches!(
            resolve_model(&state, "sonnet", None),
            Err(RelaunchError::ModelRefused(_))
        ));
    }

    #[test]
    fn a_policy_default_naming_opus_is_refused_without_the_override() {
        let state = state_with_role("overmind");
        assert!(matches!(
            resolve_model(&state, "claude-opus-5-5", None),
            Err(RelaunchError::ModelRefused(_))
        ));
    }

    #[test]
    fn the_explicit_override_allows_opus() {
        let mut state = state_with_role("overmind");
        state.launch_args = Some(strs(&["claude.exe", "--model", "opus"]));
        assert_eq!(
            resolve_model(&state, "sonnet", Some(&OpusPermit::for_test())).unwrap(),
            "opus"
        );
    }

    #[test]
    fn only_sonnet_and_haiku_names_need_no_permit_and_everything_else_fails_closed() {
        // Positive controls: the names this fleet pins.
        for ok in [
            "sonnet",
            "haiku",
            "Sonnet",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
            "sonnet[1m]",
        ] {
            assert_eq!(Model::parse(ok, None).unwrap(), ok, "{ok}");
        }
        // Opus in any spelling, and names this code has never heard of (a future tier), are refused.
        for bad in [
            "opus",
            "Claude-OPUS-5",
            "claude-opus-5-5",
            "opus[1m]",
            "claude-sonnet-opus",
            "claude-fable-5-1",
            "some-future-model",
            "",
            "sonnetx",
        ] {
            assert!(
                matches!(Model::parse(bad, None), Err(RelaunchError::ModelRefused(_))),
                "{bad:?}"
            );
            // The permit is what lets a refused name through.
            assert_eq!(
                Model::parse(bad, Some(&OpusPermit::for_test()))
                    .unwrap()
                    .as_str(),
                bad
            );
        }
    }

    #[test]
    fn the_stop_path_reads_the_recorded_model_without_a_permit_or_a_check() {
        let args = strs(&["claude.exe", "--model", "claude-opus-5-5"]);
        assert_eq!(
            recorded_launch_model(Some(&args), "sonnet"),
            "claude-opus-5-5"
        );
        assert_eq!(recorded_launch_model(None, "haiku"), "haiku");
    }

    #[test]
    fn the_opus_check_is_case_insensitive() {
        let mut state = state_with_role("overmind");
        state.launch_args = Some(strs(&["claude.exe", "--model", "Claude-OPUS-5"]));
        assert!(resolve_model(&state, "sonnet", None).is_err());
    }

    #[test]
    fn policy_default_reads_the_file_and_falls_back_to_sonnet() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(policy_default_model(dir.path()), "sonnet", "no file");
        std::fs::write(dir.path().join("model-policy.json"), r#"{"model":"haiku"}"#).unwrap();
        assert_eq!(policy_default_model(dir.path()), "haiku");
        std::fs::write(dir.path().join("model-policy.json"), "not json").unwrap();
        assert_eq!(policy_default_model(dir.path()), "sonnet", "garbage");
    }

    #[test]
    fn models_report_lists_each_lane_and_flags_opus() {
        let dir = tempfile::tempdir().unwrap();
        for (role, model) in [("a", "claude-sonnet-5-5"), ("b", "claude-opus-5-5")] {
            let mut s = state_with_role(role);
            s.model = Some(model.to_string());
            std::fs::write(
                dir.path().join(format!("{role}.json")),
                serde_json::to_string(&s).unwrap(),
            )
            .unwrap();
        }
        let report = models_report(dir.path());
        assert!(report.contains("claude-sonnet-5-5"), "{report}");
        let opus_line = report
            .lines()
            .find(|l| l.contains("claude-opus-5-5"))
            .unwrap();
        assert!(opus_line.contains("OPUS"), "{report}");
        let sonnet_line = report
            .lines()
            .find(|l| l.contains("claude-sonnet-5-5"))
            .unwrap();
        assert!(!sonnet_line.contains("OPUS"), "{report}");
    }

    // -- claude_argv ------------------------------------------------- //

    #[test]
    fn claude_argv_always_pins_the_model_and_omits_permission_mode_when_absent() {
        let mut state = state_with_role("overmind");
        state.model = None;
        state.permission_mode = None;
        let argv = claude_argv("overmind", &state, "the prompt", "sonnet");
        assert_eq!(
            argv,
            vec![
                "claude",
                "the prompt",
                "--name",
                "overmind",
                "--model",
                "sonnet"
            ]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn claude_argv_includes_remote_control_flag_when_set() {
        let mut state = state_with_role("overmind");
        state.remote_control = true;
        let argv = claude_argv("overmind", &state, "p", "sonnet");
        assert!(argv.contains(&"--remote-control".to_string()));
    }

    #[test]
    fn claude_argv_never_includes_resume() {
        // ⚠️ PM finding, 2026-09-18: --resume reloads the whole prior
        // transcript - exactly the cost a restart exists to cut.
        let state = state_with_role("overmind");
        let argv = claude_argv("overmind", &state, "p", "sonnet");
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
        let argv = claude_argv("overmind", &state, "the prompt", "sonnet");
        assert_eq!(
            argv,
            strs(&[
                "claude",
                "the prompt",
                "--name",
                "overmind",
                "--model",
                "sonnet",
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

    /// `-n` (short for `--name`) and `--name=` are recognised and
    /// dropped like `--name`: the relaunch emits a fresh `--name`. Before,
    /// `-n` was dropped as an unknown flag and its value left behind.
    #[test]
    fn the_short_and_equals_forms_of_name_are_dropped_with_their_value() {
        let (extra, dropped) = extra_launch_args(&strs(&[
            "claude.exe",
            "-n",
            "PM",
            "--name=old",
            "--settings",
            "s.json",
        ]));
        assert_eq!(extra, strs(&["--settings", "s.json"]));
        assert!(dropped.is_empty(), "reported as unknown: {dropped:?}");
    }

    #[test]
    fn claude_argv_never_duplicates_name_when_the_original_launch_already_had_one() {
        let mut state = state_with_role("overmind");
        state.launch_args = Some(strs(&["claude.exe", "--name", "old-name"]));
        let argv = claude_argv("overmind", &state, "p", "sonnet");
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
        let argv = claude_argv("overmind", &state, "p", "sonnet");
        assert_eq!(
            argv,
            strs(&[
                "claude",
                "p",
                "--name",
                "overmind",
                "--model",
                "sonnet",
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
        let argv = claude_argv("overmind", &state, "the prompt", "sonnet");

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
    fn a_hosted_argv_is_checked_whole_cwd_claude_args_and_the_prepended_host_part() {
        // Positive control: a clean command line builds, and derefs to the claude argv.
        let ok = HostedArgv::new("C:/Projects/x", "x", strs(&["claude", "go"])).unwrap();
        assert_eq!(&*ok, &strs(&["claude", "go"])[..]);
        assert_eq!(ok.cwd(), "C:/Projects/x");
        // What is spawned is the host-wrapped line, which the check covered.
        assert!(ok.hosted().ends_with(&strs(&["--", "claude", "go"])));
        assert!(ok.hosted().iter().all(|a| !a.contains(';')));
        // The same shape with a ';' in the cwd, or in a claude argument, is refused.
        for (cwd, args) in [
            ("C:/a;b", strs(&["claude"])),
            ("C:/ok", strs(&["claude", "p;q"])),
        ] {
            assert!(
                matches!(
                    HostedArgv::new(cwd, "x", args),
                    Err(RelaunchError::UnsafeArgument(_))
                ),
                "{cwd}"
            );
        }
        // The role is part of the wrapped line too (callers validate it, this does not rely on that).
        assert!(matches!(
            HostedArgv::new("C:/ok", "a;b", strs(&["claude"])),
            Err(RelaunchError::UnsafeArgument(_))
        ));
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
        // The model now comes only from the launch's explicit --model
        // (or the policy file), never from state.model.
        // A permitted prefix, so the model check passes and the `;` guard is what refuses it.
        state.launch_args = Some(strs(&["claude.exe", "--model", "claude-sonnet-5;calc"]));
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
        fn has_live_shell_descendant(&self, _: u32) -> Result<bool, crate::facts::ShellCheckError> {
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
        fn process_table(
            &self,
        ) -> Result<Vec<crate::facts::ProcEntry>, crate::facts::ShellCheckError> {
            unreachable!("the liveness wait never reads the process table")
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
