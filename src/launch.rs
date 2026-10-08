// SPDX-License-Identifier: MIT OR Apache-2.0
//! How one planned agent becomes one terminal tab (M3b): the exact argv, the safety checks that run
//! before anything starts, and the one place a process is spawned.
//!
//! The shape is `lane-restart`'s proven one (`DESIGN.md` §6; each point a past failure of
//! OverMind's):
//!
//! `wt.exe -w agentlife-<k> new-tab --title <title> -d <cwd> <host> host --role <role> -- <claude>
//! <prompt> <flags...>` and, only when `wt.exe` does not exist, `conhost.exe <host> host ...` with
//! `current_dir(<cwd>)`.
//!
//! * **A host, a real terminal**: `claude` started with inherited pipes runs as one-shot print mode and
//!   exits after the prompt. The host owns a ConPTY and answers the start-up dialog.
//! * **The prompt goes first and the flags after**: `--dangerously-load-development-channels` is
//!   variadic and would swallow a trailing prompt.
//! * **Never `--resume`**: it reloads the whole transcript, the cost a restart exists to cut.
//! * **The `;` hazard**: `wt.exe` reads `;` as its own command separator. Every element that reaches it
//!   is checked and the launch is refused *before anything starts*; one `wt.exe` is run per agent, so
//!   agentlife never needs the separator itself.
//! * **The session-identity environment is stripped**, or the new session believes it is a child of
//!   the one that launched it.
//!
//! Nothing here runs from a command in M3b: `agentlife restore` without `--dry-run` still refuses
//! (consent is M4). Tests spawn only stand-ins they built themselves.

use crate::plan::Entry;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The variables that name a running Claude Code session or its IPC channel. **Copied from OverMind**
/// (`crates/lane-restart/src/main.rs`, `SESSION_IDENTITY_ENV_VARS`; see `docs/COPIED-FROM-OVERMIND.md`
/// for the commit, hash and how to re-verify). Only variables that name *this* session are listed: a
/// user's own persistent settings (`CLAUDE_EFFORT` and the like) are left alone. Temporary, like
/// `claude_proc.rs`: it goes when OverMind publishes the crate.
pub const SESSION_IDENTITY_ENV_VARS: &[&str] = &[
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

/// The variable that carries an agent's id into its new session, so the hook joins the session to the
/// same record. Whether it survives `wt.exe` is **unverified** (the canary of M6); the hook also joins
/// by name and directory, so a lane that loses it is still found.
pub const AGENT_ID_ENV: &str = "AGENTLIFE_AGENT_ID";

/// What to run, taken from the configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Programs {
    pub host: String,
    pub claude: String,
}

/// One tab, ready to spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabLaunch {
    pub agent_id: String,
    /// `agentlife-<k>`.
    pub window: String,
    pub title: String,
    pub cwd: String,
    /// `[host, "host", "--role", role, "--", claude, prompt, flags...]`.
    pub hosted_argv: Vec<String>,
    /// Variables to set (the agent id).
    pub env_set: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildError {
    /// An element that `wt.exe` would read as a command separator (or a control character).
    Unsafe { what: String },
    /// A role that is not a plain identifier.
    BadRole(String),
    /// No `--model`, or one that is not a Sonnet or a Haiku (CireSnave, 2026-10-08: never Opus; the
    /// default model is not trusted). The planner pins one; this is the last gate before a spawn.
    ModelNotPinned { found: Option<String> },
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::Unsafe { what } => write!(
                f,
                "refusing to launch: {what:?} is unsafe for wt.exe's own argument parser"
            ),
            BuildError::BadRole(r) => {
                write!(f, "refusing to launch: {r:?} is not a valid role name")
            }
            BuildError::ModelNotPinned { found } => write!(
                f,
                "refusing to launch: the model must be explicit and a Sonnet or Haiku (found {})",
                found.as_deref().unwrap_or("none")
            ),
        }
    }
}

fn valid_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The name the host files this agent under: its name, else its id.
pub fn role_for(e: &Entry) -> String {
    e.name.clone().unwrap_or_else(|| e.agent_id.clone())
}

/// What a restored agent is told first. With a HANDOFF it continues from it; without one it says so
/// and waits, rather than inventing work (`DESIGN.md` §7: *never* `Skipped:no-handoff`).
pub fn prompt_for(role: &str, handoff_exists: bool) -> String {
    if handoff_exists {
        format!("read {role} HANDOFF and continue")
    } else {
        format!("there is no HANDOFF for {role}, so say so and wait for instructions")
    }
}

/// `[claude, prompt, flags...]`: the prompt first, the flags after.
pub fn claude_argv(claude: &str, prompt: &str, flags: &[String]) -> Vec<String> {
    let mut argv = vec![claude.to_string(), prompt.to_string()];
    argv.extend(flags.iter().cloned());
    argv
}

/// `[host, "host", "--role", role, "--", argv...]`.
pub fn hosted_argv(host: &str, role: &str, claude: &[String]) -> Vec<String> {
    let mut argv = vec![
        host.to_string(),
        "host".to_string(),
        "--role".to_string(),
        role.to_string(),
        "--".to_string(),
    ];
    argv.extend(claude.iter().cloned());
    argv
}

/// The first element `wt.exe` must not be given: a `;` (its command separator) or a control
/// character. Every element that reaches `wt.exe` goes through this, `cwd` and the title included.
pub fn first_unsafe_argument<'a>(elements: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    elements
        .into_iter()
        .find(|e| e.contains(';') || e.chars().any(char::is_control))
}

/// Builds the tab for one planned entry, or refuses **before anything starts**.
pub fn build_tab(
    e: &Entry,
    programs: &Programs,
    handoff_exists: bool,
) -> Result<TabLaunch, BuildError> {
    let role = role_for(e);
    if !valid_identifier(&role) {
        return Err(BuildError::BadRole(role));
    }
    let prompt = prompt_for(&role, handoff_exists);
    let claude = claude_argv(&programs.claude, &prompt, &e.argv);
    let hosted = hosted_argv(&programs.host, &role, &claude);
    let window = format!("agentlife-{}", e.window);
    let all: Vec<&str> = hosted
        .iter()
        .map(String::as_str)
        .chain([window.as_str(), e.title.as_str(), e.cwd.as_str()])
        .collect();
    if let Some(bad) = first_unsafe_argument(all) {
        return Err(BuildError::Unsafe {
            what: bad.to_string(),
        });
    }
    let model = effective_model(&e.argv);
    if !model.as_deref().is_some_and(crate::plan::model_is_allowed) {
        return Err(BuildError::ModelNotPinned { found: model });
    }
    Ok(TabLaunch {
        agent_id: e.agent_id.clone(),
        window,
        title: e.title.clone(),
        cwd: e.cwd.clone(),
        hosted_argv: hosted,
        env_set: vec![(AGENT_ID_ENV.to_string(), e.agent_id.clone())],
    })
}

/// The model claude will use: the value of the last model flag, in either form (`--model v` or
/// `--model=v`). `None` for no model flag, or a last one with no value.
fn effective_model(argv: &[String]) -> Option<String> {
    let mut model = None;
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        if a == "--model" {
            model = it.next().cloned();
        } else if let Some(v) = a.strip_prefix("--model=") {
            model = Some(v.to_string());
        }
    }
    model
}

/// The arguments for `wt.exe`: one tab in the named window.
pub fn wt_args(t: &TabLaunch) -> Vec<String> {
    let mut a: Vec<String> = [
        "-w", &t.window, "new-tab", "--title", &t.title, "-d", &t.cwd,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    a.extend(t.hosted_argv.iter().cloned());
    a
}

/// How a tab was started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpawnVia {
    WindowsTerminal,
    /// `wt.exe` is absent: one console window per agent, no tabs. The agent still starts.
    Conhost,
    /// A test double that ran the host directly (never a real lane).
    Direct,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnError(pub String);

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The one thing that starts a process. A test supplies its own and spawns stand-ins.
pub trait Spawner {
    fn spawn(&self, tab: &TabLaunch) -> Result<SpawnVia, SpawnError>;
}

fn strip_session_identity_env(cmd: &mut Command) {
    for var in SESSION_IDENTITY_ENV_VARS {
        cmd.env_remove(var);
    }
}

/// Starts a tab with `wt.exe`, falling back to `conhost.exe` only when `wt.exe` **does not exist**
/// (a `NotFound` spawn error, never a guess from `PATH`). Any other spawn error is a failure, not a
/// reason to try something else.
#[derive(Debug, Clone)]
pub struct RealSpawner {
    pub wt: String,
    pub conhost: String,
    /// Extra environment for the spawned process (a test points it at its own `AGENTLIFE_HOME`).
    pub extra_env: Vec<(String, String)>,
    /// How many times to start `conhost.exe` before giving up (see [`start_settled`]).
    pub conhost_attempts: u32,
    /// How long a started `conhost.exe` is given to have its command running as a child.
    pub conhost_settle: Duration,
}

impl Default for RealSpawner {
    fn default() -> Self {
        Self {
            wt: "wt.exe".to_string(),
            conhost: "conhost.exe".to_string(),
            extra_env: Vec::new(),
            conhost_attempts: 5,
            conhost_settle: Duration::from_secs(8),
        }
    }
}

impl RealSpawner {
    fn prepare(&self, cmd: &mut Command, tab: &TabLaunch) {
        strip_session_identity_env(cmd);
        for (k, v) in self.extra_env.iter().chain(tab.env_set.iter()) {
            cmd.env(k, v);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    }

    /// The `wt.exe` command for a tab, not yet started.
    pub fn wt_command(&self, tab: &TabLaunch) -> Command {
        let mut c = Command::new(&self.wt);
        c.args(wt_args(tab));
        self.prepare(&mut c, tab);
        c
    }

    /// The `conhost.exe` fallback command for a tab, not yet started.
    pub fn conhost_command(&self, tab: &TabLaunch) -> Command {
        let mut c = Command::new(&self.conhost);
        c.args(&tab.hosted_argv);
        c.current_dir(&tab.cwd);
        self.prepare(&mut c, tab);
        c
    }
}

impl Spawner for RealSpawner {
    fn spawn(&self, tab: &TabLaunch) -> Result<SpawnVia, SpawnError> {
        match self.wt_command(tab).spawn() {
            Ok(_) => return Ok(SpawnVia::WindowsTerminal),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(SpawnError(format!("{}: {e}", self.wt))),
        }
        let host_name = std::path::Path::new(&tab.hosted_argv[0])
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // The host must be seen **and still there a moment later**: on 2026-10-07 a freshly started
        // host was seen as `conhost.exe`'s child and was gone within 200 ms, without having run.
        let sustain = Duration::from_secs(1);
        let first_seen: std::cell::Cell<Option<Instant>> = std::cell::Cell::new(None);
        start_settled(
            self.conhost_attempts,
            self.conhost_settle,
            || {
                first_seen.set(None);
                self.conhost_command(tab).spawn()
            },
            |child, _| {
                if has_child_named(child.id(), &host_name) {
                    let since = first_seen.get().unwrap_or_else(Instant::now);
                    first_seen.set(Some(since));
                    since.elapsed() >= sustain
                } else {
                    first_seen.set(None);
                    false
                }
            },
        )
        .map(|_| SpawnVia::Conhost)
        .map_err(|e| {
            SpawnError(format!(
                "{} (after {} was not found): {e}",
                self.conhost, self.wt
            ))
        })
    }
}

/// Whether a process named `name` (compared without case, with or without `.exe`) has `parent` as
/// its parent: `conhost.exe` has started *its command*, not merely some helper of its own.
fn has_child_named(parent: u32, name: &str) -> bool {
    let want = name.to_ascii_lowercase();
    let want = want.strip_suffix(".exe").unwrap_or(&want).to_string();
    let mut sys = sysinfo::System::new();
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        sysinfo::ProcessRefreshKind::nothing(),
    );
    sys.processes().values().any(|p| {
        p.parent().is_some_and(|pp| pp.as_u32() == parent) && {
            let n = p.name().to_string_lossy().to_ascii_lowercase();
            n.strip_suffix(".exe").unwrap_or(&n) == want
        }
    })
}

/// Starts a process and only believes it once `started` says its job has begun; one that exits
/// first, or has not begun by `settle`, is ended and started again, up to `attempts` times.
///
/// Why: `conhost.exe` sometimes does not run its command. Measured on 2026-10-07 on a busy machine:
/// back-to-back starts lost one of two about half of the time; the failures exited within 0.2 to 0.8 s
/// or lingered and then went, and left no trace of the host, while the ones that worked had the host
/// running as a child within about 0.3 s. Without this the agent simply never appears and the run
/// learns it only when the liveness timeout passes. Starting again is safe: a `conhost.exe` that never
/// ran the host has nothing to duplicate, and one that is ended before it did is ended with its console.
/// A start that cannot be attempted at all (a spawn error) is final and is never retried.
pub fn start_settled(
    attempts: u32,
    settle: Duration,
    mut start: impl FnMut() -> std::io::Result<Child>,
    mut started: impl FnMut(&Child, Duration) -> bool,
) -> Result<Child, String> {
    let attempts = attempts.max(1);
    let mut last = String::new();
    for _ in 1..=attempts {
        let mut child = start().map_err(|e| e.to_string())?;
        let began = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    last = format!("it exited after {:?} with {status}", began.elapsed());
                    break;
                }
                Ok(None) if started(&child, began.elapsed()) => return Ok(child),
                Ok(None) if began.elapsed() >= settle => {
                    last = format!("it had not started its command after {settle:?}");
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => return Err(e.to_string()),
            }
        }
    }
    Err(format!(
        "it did not start its command on any of {attempts} attempts (last: {last})"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: Option<&str>, window: u32) -> Entry {
        Entry {
            agent_id: "a-7".into(),
            name: name.map(String::from),
            title: name.unwrap_or("a-7").to_string(),
            cwd: "C:/Projects/lane".into(),
            argv: [
                "--name",
                "lane",
                "--model",
                "sonnet",
                "--dangerously-load-development-channels",
                "server:claude-peers",
            ]
            .map(String::from)
            .to_vec(),
            mode: None,
            pm: false,
            flags: vec![],
            dropped_args: vec![],
            batch: 0,
            window,
            tab: 0,
        }
    }

    fn programs() -> Programs {
        Programs {
            host: "lane-restart".into(),
            claude: "claude".into(),
        }
    }

    #[test]
    fn the_prompt_comes_first_the_flags_after_and_there_is_never_a_resume() {
        let t = build_tab(&entry(Some("lane"), 2), &programs(), true).unwrap();
        assert_eq!(
            t.hosted_argv,
            [
                "lane-restart",
                "host",
                "--role",
                "lane",
                "--",
                "claude",
                "read lane HANDOFF and continue",
                "--name",
                "lane",
                "--model",
                "sonnet",
                "--dangerously-load-development-channels",
                "server:claude-peers"
            ]
        );
        assert!(t.hosted_argv.iter().all(|a| !a.contains("resume")));
        // Nothing positional follows the variadic flag's value.
        assert_eq!(t.hosted_argv.last().unwrap(), "server:claude-peers");
        assert_eq!(t.window, "agentlife-2");
        assert_eq!(
            t.env_set,
            [("AGENTLIFE_AGENT_ID".to_string(), "a-7".to_string())]
        );
    }

    #[test]
    fn wt_gets_one_tab_in_the_named_window_with_the_hosted_command_last() {
        let t = build_tab(&entry(Some("lane"), 0), &programs(), true).unwrap();
        let a = wt_args(&t);
        assert_eq!(
            a[..7],
            [
                "-w",
                "agentlife-0",
                "new-tab",
                "--title",
                "lane",
                "-d",
                "C:/Projects/lane"
            ]
        );
        assert_eq!(a[7..], t.hosted_argv[..]);
        assert!(
            a.iter().all(|s| s != ";"),
            "agentlife never chains with the separator"
        );
    }

    #[test]
    fn a_missing_handoff_is_said_not_skipped() {
        let t = build_tab(&entry(Some("lane"), 0), &programs(), false).unwrap();
        assert!(t.hosted_argv[6].contains("no HANDOFF for lane"));
    }

    #[test]
    fn an_unnamed_agent_is_filed_under_its_id() {
        let t = build_tab(&entry(None, 0), &programs(), true).unwrap();
        assert_eq!(t.hosted_argv[3], "a-7");
        assert_eq!(t.title, "a-7");
    }

    #[test]
    fn a_semicolon_or_control_character_anywhere_refuses_before_anything_starts() {
        let mut cases: Vec<(&str, Entry, Programs)> = Vec::new();
        let mut e = entry(Some("lane"), 0);
        e.cwd = "C:/Projects/a;b".into();
        cases.push(("cwd", e, programs()));
        let mut e = entry(Some("lane"), 0);
        e.title = "x;y".into();
        cases.push(("title", e, programs()));
        let mut e = entry(Some("lane"), 0);
        e.argv.push("--model".into());
        e.argv.push("a;b".into());
        cases.push(("flag value", e, programs()));
        let mut e = entry(Some("lane"), 0);
        e.argv.push("tab\there".into());
        cases.push(("control char", e, programs()));
        let p = Programs {
            host: "C:/x;y/host".into(),
            claude: "claude".into(),
        };
        cases.push(("host", entry(Some("lane"), 0), p));
        let p = Programs {
            host: "h".into(),
            claude: "c;d".into(),
        };
        cases.push(("claude", entry(Some("lane"), 0), p));
        for (what, e, p) in cases {
            assert!(
                matches!(build_tab(&e, &p, true), Err(BuildError::Unsafe { .. })),
                "{what}"
            );
        }
    }

    #[test]
    fn a_launch_without_an_allowed_explicit_model_is_refused_before_anything_starts() {
        let without = |mut e: Entry| {
            let i = e.argv.iter().position(|a| a == "--model").unwrap();
            e.argv.drain(i..i + 2);
            e
        };
        let with = |model: &str| {
            let mut e = without(entry(Some("lane"), 0));
            e.argv.extend(["--model".to_string(), model.to_string()]);
            e
        };
        let mut cases = vec![
            ("no model flag", without(entry(Some("lane"), 0))),
            ("opus id", with("claude-opus-5-5")),
            ("opus alias", with("opus")),
            ("unknown", with("fable")),
        ];
        // The last `--model` is the one claude uses: an opus after a sonnet still refuses.
        let mut e = entry(Some("lane"), 0);
        e.argv.extend(["--model".to_string(), "opus".to_string()]);
        cases.push(("opus last", e));
        // The equals form is a model flag too, and the last one of either form wins.
        for (what, extra) in [
            ("opus equals after sonnet pair", vec!["--model=opus"]),
            ("upper-case equals", vec!["--model=CLAUDE-OPUS-5-5"]),
            ("mixed case pair", vec!["--model", "Opus"]),
            ("equals unknown", vec!["--model=fable"]),
            ("empty equals", vec!["--model="]),
            ("dangling flag", vec!["--model"]),
        ] {
            let mut e = entry(Some("lane"), 0);
            e.argv.extend(extra.iter().map(|s| s.to_string()));
            cases.push((what, e));
        }
        for (what, e) in cases {
            assert!(
                matches!(
                    build_tab(&e, &programs(), true),
                    Err(BuildError::ModelNotPinned { .. })
                ),
                "{what}"
            );
        }
        for ok in ["sonnet", "haiku", "claude-sonnet-5-5"] {
            assert!(build_tab(&with(ok), &programs(), true).is_ok(), "{ok}");
        }
        // Equals form of an allowed model, after an opus pair: the last one wins, so it launches.
        let mut e = entry(Some("lane"), 0);
        e.argv
            .extend(["--model", "opus", "--model=HAIKU"].map(String::from));
        assert!(build_tab(&e, &programs(), true).is_ok());
    }

    #[test]
    fn a_role_that_is_not_a_plain_identifier_is_refused() {
        for bad in ["a&calc", "a b", "", "x/y", &"x".repeat(65)] {
            let mut e = entry(Some("lane"), 0);
            e.name = Some(bad.to_string());
            assert!(
                matches!(
                    build_tab(&e, &programs(), true),
                    Err(BuildError::BadRole(_))
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn the_session_identity_variables_are_the_ones_that_name_this_session() {
        assert_eq!(SESSION_IDENTITY_ENV_VARS.len(), 10);
        assert!(SESSION_IDENTITY_ENV_VARS.contains(&"CLAUDE_CODE_CHILD_SESSION"));
        for keep in ["CLAUDE_EFFORT", "CLAUDE_CODE_EXECPATH", "AGENTLIFE_HOME"] {
            assert!(!SESSION_IDENTITY_ENV_VARS.contains(&keep), "{keep}");
        }
    }

    #[test]
    fn both_commands_strip_the_session_identity_and_set_the_agent_id_and_conhost_sets_the_cwd() {
        let s = RealSpawner {
            extra_env: vec![("AGENTLIFE_HOME".into(), "D:/h".into())],
            ..RealSpawner::default()
        };
        let t = build_tab(&entry(Some("lane"), 1), &programs(), true).unwrap();
        for (what, cmd) in [("wt", s.wt_command(&t)), ("conhost", s.conhost_command(&t))] {
            let envs: std::collections::HashMap<_, _> = cmd
                .get_envs()
                .map(|(k, v)| {
                    (
                        k.to_string_lossy().into_owned(),
                        v.map(|v| v.to_string_lossy().into_owned()),
                    )
                })
                .collect();
            for var in SESSION_IDENTITY_ENV_VARS {
                assert_eq!(envs.get(*var), Some(&None), "{what} must remove {var}");
            }
            assert_eq!(envs["AGENTLIFE_AGENT_ID"].as_deref(), Some("a-7"), "{what}");
            assert_eq!(envs["AGENTLIFE_HOME"].as_deref(), Some("D:/h"), "{what}");
        }
        assert_eq!(s.wt_command(&t).get_program(), "wt.exe");
        assert_eq!(s.wt_command(&t).get_args().count(), wt_args(&t).len());
        assert_eq!(s.conhost_command(&t).get_program(), "conhost.exe");
        assert_eq!(
            s.conhost_command(&t)
                .get_current_dir()
                .map(|p| p.to_string_lossy().into_owned()),
            Some("C:/Projects/lane".to_string())
        );
        assert_eq!(
            s.conhost_command(&t)
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            t.hosted_argv
        );
    }

    #[test]
    fn a_wt_that_exists_but_cannot_run_is_a_failure_not_a_reason_to_try_conhost() {
        // A directory is "found" but cannot be run: the error must name wt and never reach conhost.
        let dir = tempfile::tempdir().unwrap();
        let s = RealSpawner {
            wt: dir.path().display().to_string(),
            conhost: "agentlife-no-such-conhost".into(),
            ..RealSpawner::default()
        };
        let t = build_tab(&entry(Some("lane"), 0), &programs(), true).unwrap();
        let err = s.spawn(&t).unwrap_err().to_string();
        assert!(
            !err.contains("no-such-conhost"),
            "fell through to conhost: {err}"
        );
    }

    /// A process that exits at once, and one that lives for several seconds.
    fn quick_exit() -> std::io::Result<Child> {
        #[cfg(windows)]
        let mut c = {
            let mut c = Command::new("cmd");
            c.args(["/C", "exit 0"]);
            c
        };
        #[cfg(not(windows))]
        let mut c = Command::new("true");
        c.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    }

    fn stays_alive() -> std::io::Result<Child> {
        #[cfg(windows)]
        let mut c = {
            let mut c = Command::new("ping");
            c.args(["-n", "6", "127.0.0.1"]);
            c
        };
        #[cfg(not(windows))]
        let mut c = {
            let mut c = Command::new("sleep");
            c.arg("5");
            c
        };
        c.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    }

    #[test]
    fn a_process_that_exits_at_once_is_started_again_and_one_that_stays_is_believed() {
        // "Started" is decided by which kind of process it is, not by a timing guess, so a busy
        // machine cannot turn a slow-to-exit process into a believed one.
        let settle = Duration::from_secs(5);
        let stayers = std::cell::RefCell::new(std::collections::HashSet::new());
        let is_stayer = |c: &Child, _: Duration| stayers.borrow().contains(&c.id());
        let start_stayer = || {
            let c = stays_alive()?;
            stayers.borrow_mut().insert(c.id());
            Ok(c)
        };
        // Exits twice, then stays: believed on the third attempt, after exactly three starts.
        let mut starts = 0;
        let mut child = start_settled(
            5,
            settle,
            || {
                starts += 1;
                if starts < 3 {
                    quick_exit()
                } else {
                    start_stayer()
                }
            },
            is_stayer,
        )
        .expect("the third start stays alive");
        assert_eq!(starts, 3);
        let _ = child.kill();
        let _ = child.wait();
        // Stays at once: one start.
        let mut starts = 0;
        let mut child = start_settled(
            5,
            settle,
            || {
                starts += 1;
                start_stayer()
            },
            is_stayer,
        )
        .unwrap();
        assert_eq!(starts, 1);
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn a_process_that_lives_but_never_starts_its_job_is_ended_and_started_again() {
        // Alive, but the predicate never says "started": each attempt is ended at the deadline.
        let mut starts = 0;
        let mut ended = Vec::new();
        let err = start_settled(
            2,
            Duration::from_millis(300),
            || {
                starts += 1;
                stays_alive()
            },
            |c, _| {
                ended.push(c.id());
                false
            },
        )
        .unwrap_err();
        assert_eq!(starts, 2);
        assert!(err.contains("had not started its command"), "{err}");
        // Each of them really was ended: its pid is no longer in the table as the same process.
        std::thread::sleep(Duration::from_millis(200));
        ended.sort();
        ended.dedup();
        assert_eq!(ended.len(), 2);
        for pid in ended {
            assert!(
                crate::identity::ProcessTable::identity_of(&crate::identity::SysinfoTable, pid)
                    .is_none(),
                "pid {pid} is still alive"
            );
        }
    }

    #[test]
    fn a_process_that_always_exits_is_given_up_on_after_the_attempts_and_says_so() {
        let never = |_: &Child, _| false;
        let mut starts = 0;
        let err = start_settled(
            3,
            Duration::from_millis(300),
            || {
                starts += 1;
                quick_exit()
            },
            never,
        )
        .unwrap_err();
        assert_eq!(starts, 3);
        assert!(err.contains("any of 3 attempts"), "{err}");
        // A start that cannot even be attempted is not retried: that is a real failure.
        let mut starts = 0;
        let err = start_settled(
            3,
            Duration::from_millis(300),
            || {
                starts += 1;
                Command::new("agentlife-no-such-program-anywhere").spawn()
            },
            never,
        )
        .unwrap_err();
        assert_eq!(starts, 1, "a spawn error is final");
        assert!(!err.contains("attempts"), "{err}");
        // Zero attempts still means one.
        let mut starts = 0;
        let _ = start_settled(
            0,
            Duration::from_millis(100),
            || {
                starts += 1;
                quick_exit()
            },
            never,
        );
        assert_eq!(starts, 1);
    }

    #[test]
    fn has_child_named_sees_a_real_child_by_its_own_name_and_not_another_or_a_leaf() {
        let mut c = stays_alive().unwrap();
        let me = std::process::id();
        let name = if cfg!(windows) { "PING.EXE" } else { "sleep" };
        // The child is there under its name, with or without the suffix and whatever the case...
        let deadline = Instant::now() + Duration::from_secs(5);
        while !has_child_named(me, name) {
            assert!(
                Instant::now() < deadline,
                "the child never showed in the table"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(has_child_named(me, &name.to_ascii_lowercase()));
        if cfg!(windows) {
            assert!(has_child_named(me, "ping"));
        }
        // ...but a helper with another name does not count, and a leaf has no children.
        assert!(!has_child_named(me, "agentlife-no-such-name"));
        assert!(!has_child_named(c.id(), name));
        let _ = c.kill();
        let _ = c.wait();
    }

    #[test]
    fn the_real_spawner_falls_back_to_conhost_only_when_wt_does_not_exist() {
        // Neither program exists: the error names both, in the order tried.
        let s = RealSpawner {
            wt: "agentlife-no-such-wt".into(),
            conhost: "agentlife-no-such-conhost".into(),
            ..RealSpawner::default()
        };
        let t = build_tab(&entry(Some("lane"), 0), &programs(), true).unwrap();
        let err = s.spawn(&t).unwrap_err().to_string();
        assert!(
            err.contains("agentlife-no-such-conhost")
                && err.contains("agentlife-no-such-wt was not found"),
            "{err}"
        );
    }
}
