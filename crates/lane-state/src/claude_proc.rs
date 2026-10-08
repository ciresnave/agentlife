// SPDX-License-Identifier: MIT OR Apache-2.0

//! Finding a lane's `claude` process and reading its launch command line:
//! the pure rules (no I/O of their own) that lane-restart's hook command and
//! agentlife both need. Moved unchanged from lane-restart's
//! `lane_state_writer` (RESTART-TOOL-DESIGN.md section 10), with the two
//! visibility changes agentlife's copy had: `ModelField::into_string` and
//! `transcript_project_dir` are `pub`.

use crate::state::LaneState;
use serde::Deserialize;

/// PM finding, 2026-09-18 (fourth interactive retest): `model` IS present in
/// a real `SessionStart` payload's key list - but only the key names were
/// logged, not the value's shape. ⚠️ NOT CONFIRMED: this crate's own headless
/// probe (an ephemeral `claude -p` session) doesn't include `model` in
/// `SessionStart` at all, the same headless/interactive gap §10.3 already
/// found once - so the shape below is a defensive guess, not a verified
/// fact, until a real interactive capture confirms it. Accepts either a
/// plain string or an object carrying an `id` field, so a future confirmed
/// shape doesn't need a schema-breaking follow-up either way.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum ModelField {
    Plain(String),
    WithId { id: String },
}

impl ModelField {
    pub fn into_string(self) -> String {
        match self {
            ModelField::Plain(s) => s,
            ModelField::WithId { id } => id,
        }
    }
}

/// The subset of hooks.md's documented common input fields this command
/// reads. Unknown fields are ignored by `serde_json`'s default behaviour -
/// no `deny_unknown_fields` here, since a future Claude Code version adding
/// fields must not break this parse.
///
/// ⚠️ REVISED (PM finding, 2026-09-18, third interactive retest): a real
/// `SessionStart` payload does NOT include `permission_mode` - parsing
/// failed with "missing field `permission_mode`" against live input, not a
/// hypothetical. "Documented common field" is not the same claim as
/// "present on every event, always" - hooks.md's own table never promised
/// that. Only `hook_event_name`, `session_id`, and `cwd` are still required;
/// everything else this struct reads is `Option<T>`, absent rather than
/// guessed when an event's real payload doesn't include it.
#[derive(Debug, Deserialize)]
pub struct HookInput {
    pub hook_event_name: String,
    pub session_id: String,
    pub cwd: String,
    pub permission_mode: Option<String>,
    pub model: Option<ModelField>,
    /// `~/.claude/projects/<launch dir, encoded>/<session_id>.jsonl` - the
    /// one field that still names the LAUNCH directory after the session
    /// has moved on, used only to cross-check which cwd to record.
    pub transcript_path: Option<String>,
}

#[derive(Debug)]
pub enum PidError {
    ParentNotFound,
    /// A non-shell, non-`claude` image appeared before `claude` was found -
    /// refused rather than skipped, unlike a shell hop.
    UnexpectedAncestor(String),
    /// Walked `MAX_HOPS` shell layers without finding `claude` - refused
    /// rather than walking forever.
    HopLimitExceeded,
}

impl std::fmt::Display for PidError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PidError::ParentNotFound => write!(f, "could not find this hook's parent process"),
            PidError::UnexpectedAncestor(name) => {
                write!(
                    f,
                    "ancestor process is {name:?} - neither claude nor a known shell, \
                     refusing to record a wrong pid"
                )
            }
            PidError::HopLimitExceeded => {
                write!(
                    f,
                    "no claude process found within {MAX_ANCESTRY_HOPS} shell hops"
                )
            }
        }
    }
}

/// The `claude` process's own pid, from the CURRENTLY RUNNING HOOK's
/// ancestry - hooks.md's common input fields do NOT include one directly
/// (RESTART-TOOL-DESIGN.md §10.1), so this derives it instead of trusting
/// anything the hook input claims.
pub trait ParentProcess {
    /// `(parent_pid, parent_image_name)` of the process identified by `pid`.
    fn parent_of(&self, pid: u32) -> Option<(u32, String)>;

    /// The full argv of the process identified by `pid`, if it could be
    /// read. Used only against the `claude` pid `claude_parent_pid` already
    /// found - never against an unverified process.
    fn cmdline_of(&self, pid: u32) -> Option<Vec<String>>;

    /// When the process identified by `pid` started, in seconds since the
    /// epoch, if it could be read.
    fn start_time_of(&self, _pid: u32) -> Option<u64> {
        None
    }
}

/// What `claude`'s own launch command line says, that no hook field
/// carries. PM finding, 2026-09-18 (fourth interactive retest): a session
/// launched with `--remote-control` still recorded `remote_control: false`,
/// because no `hooks.md` field reports it at all. Read from the pid
/// `claude_parent_pid` already verified, not guessed and not left as
/// another "unknown, treated as safe" gap.
///
/// ⚠️ KNOWN LIMIT (PM finding, 2026-09-18, post-merge gate read): CireSnave's
/// own settings carry `remoteControlAtStartup: true`, so a lane can get
/// Remote Control with no `--remote-control` flag on its command line at
/// all - `remote_control` then reads `false` here even though the session
/// genuinely has it. Not wrong *in practice*: a relaunch picks Remote
/// Control back up from that same setting regardless of what this field
/// says. But the state file itself doesn't prove it either way. Low
/// priority - RESTART-TOOL-DESIGN.md §10.1.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaudeCliFlags {
    pub remote_control: bool,
    /// `--name`, `--name=` or `-n`: how a person recognises the session.
    pub name: Option<String>,
    pub permission_mode: Option<String>,
    /// The full argv this was parsed from, retained verbatim - PM finding,
    /// 2026-09-18 (CireSnave, via the PM): a relaunch that only reconstructs
    /// `--model`/`--permission-mode`/`--remote-control` silently drops
    /// every other real launch flag (CireSnave's own lanes always carry
    /// `--dangerously-load-development-channels server:claude-peers`,
    /// without which a relaunched lane can send but never RECEIVE
    /// `claude-peers` notifications). `main.rs`'s relaunch rebuilds its
    /// own argv from an ALLOWLIST parsed out of this, never passed
    /// through blindly.
    pub launch_args: Option<Vec<String>>,
}

/// Pure - given a command line, no I/O. `--dangerously-skip-permissions`
/// maps to Claude Code's own name for that mode (`bypassPermissions`,
/// confirmed in `sessions.md`'s permission-mode table), not a guessed
/// string. Accepts `--permission-mode value` and `--permission-mode=value`
/// both (PM finding, 2026-09-18, post-merge gate read: the equals form
/// wasn't parsed).
pub fn parse_claude_cli_flags(cmdline: &[String]) -> ClaudeCliFlags {
    let mut flags = ClaudeCliFlags {
        launch_args: Some(cmdline.to_vec()),
        ..ClaudeCliFlags::default()
    };
    let mut iter = cmdline.iter();
    while let Some(arg) = iter.next() {
        if let Some(v) = arg.strip_prefix("--permission-mode=") {
            flags.permission_mode = Some(v.to_string());
            continue;
        }
        if let Some(v) = arg.strip_prefix("--name=") {
            flags.name = Some(v.to_string());
            continue;
        }
        match arg.as_str() {
            "--remote-control" => flags.remote_control = true,
            "--name" | "-n" => {
                if let Some(v) = iter.next() {
                    flags.name = Some(v.clone());
                }
            }
            "--dangerously-skip-permissions" => {
                flags.permission_mode = Some("bypassPermissions".to_string())
            }
            "--permission-mode" => {
                if let Some(v) = iter.next() {
                    flags.permission_mode = Some(v.clone());
                }
            }
            _ => {}
        }
    }
    flags
}

/// Image names (lowercased, no `.exe`) a hop is allowed to pass through
/// without stopping. PM finding, 2026-09-18, from a REAL interactive
/// session: Claude Code ran a shell-form hook command through Git Bash,
/// two layers deep (`hook <- bash <- bash <- claude.exe`) - the DIRECT
/// parent was `bash.exe`, not `claude.exe`, so the single-hop check this
/// module shipped with (verified only against a headless `claude -p`
/// session, §10.3) refused every real interactive write.
const SHELL_ANCESTOR_NAMES: &[&str] = &["bash", "sh", "cmd", "powershell", "pwsh"];

/// How many shell layers this walk tolerates before giving up -
/// RESTART-TOOL-DESIGN.md §11.1's real chain needed 2; this leaves margin
/// without walking indefinitely.
const MAX_ANCESTRY_HOPS: u32 = 4;

fn image_base(name: &str) -> String {
    let lower = name.to_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}

/// Walks up from `my_pid`, skipping ONLY known shell images, and returns
/// the pid of the first `claude` found. Refuses (never skips past) any
/// ancestor that is neither a known shell nor `claude` - a stranger
/// appearing before `claude` is exactly what this check exists to catch,
/// not something to tolerate the way a shell hop is tolerated.
pub fn claude_parent_pid(my_pid: u32, lookup: &dyn ParentProcess) -> Result<u32, PidError> {
    let mut current = my_pid;
    for _ in 0..MAX_ANCESTRY_HOPS {
        let (parent_pid, name) = lookup.parent_of(current).ok_or(PidError::ParentNotFound)?;
        let base = image_base(&name);
        if base == "claude" {
            return Ok(parent_pid);
        }
        if !SHELL_ANCESTOR_NAMES.contains(&base.as_str()) {
            return Err(PidError::UnexpectedAncestor(name));
        }
        current = parent_pid;
    }
    Err(PidError::HopLimitExceeded)
}

/// The parent directory's own name in a transcript path, either separator.
pub fn transcript_project_dir(transcript_path: &str) -> Option<&str> {
    let mut parts = transcript_path.rsplit(['/', '\\']);
    parts.next()?;
    parts.next().filter(|d| !d.is_empty())
}

/// The cwd a state file records: the lane's HOME (its launch directory),
/// which `decide`'s transcript check, the relaunch and the liveness check
/// all key on - never merely where the session happens to be right now.
/// PM finding, 2026-10-02: `SessionStart` also fires on a compaction,
/// mid-session, with the CURRENT cwd, and recording that sent the PM's own
/// restart looking in the wrong project directory.
///
/// The candidates, in order: the cwd already recorded by this SAME process
/// (a process's launch directory never changes), then this event's own cwd.
/// When `transcript_path` is present, the first candidate whose encoding
/// matches the transcript's own directory wins. When none matches, the
/// order alone decides and nothing is guessed - `decide`'s transcript check
/// then refuses rather than relaunching in the wrong place.
pub fn recorded_cwd(existing: Option<&LaneState>, input: &HookInput, pid: u32) -> String {
    let candidates: Vec<&str> = existing
        .filter(|s| s.pid == pid)
        .map(|s| s.cwd.as_str())
        .into_iter()
        .chain(std::iter::once(input.cwd.as_str()))
        .collect();
    let by_transcript = input
        .transcript_path
        .as_deref()
        .and_then(transcript_project_dir)
        .and_then(|dir| {
            candidates
                .iter()
                .find(|c| crate::paths::project_dir_name(c).eq_ignore_ascii_case(dir))
        });
    by_transcript.unwrap_or(&candidates[0]).to_string()
}

/// Environment variables that name a running Claude Code session or its IPC
/// channel; a relaunch must not hand them on. (Moved here from lane-restart's
/// `relaunch` module, with its tests left beside the code that strips them.)
///
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

#[cfg(test)]
mod tests {
    use super::*;

    // -- claude_parent_pid ----------------------------------------------------- //
    // PM finding, 2026-09-18, from a REAL interactive session: the direct parent
    // was `bash.exe`, not `claude.exe` - `hook <- bash <- bash <- claude.exe`.
    // Every case here uses a pid-keyed chain so a fake can express that shape,
    // not just a single hop.

    struct FakeAncestry(std::collections::HashMap<u32, (u32, String)>);
    impl FakeAncestry {
        fn chain(links: &[(u32, u32, &str)]) -> Self {
            let mut map = std::collections::HashMap::new();
            for (pid, parent_pid, parent_name) in links {
                map.insert(*pid, (*parent_pid, parent_name.to_string()));
            }
            Self(map)
        }
    }
    impl ParentProcess for FakeAncestry {
        fn parent_of(&self, pid: u32) -> Option<(u32, String)> {
            self.0.get(&pid).cloned()
        }
        fn cmdline_of(&self, _pid: u32) -> Option<Vec<String>> {
            None
        }
    }

    #[test]
    fn accepts_a_direct_claude_parent() {
        let lookup = FakeAncestry::chain(&[(1, 99, "claude")]);
        assert_eq!(claude_parent_pid(1, &lookup).unwrap(), 99);
    }

    #[test]
    fn accepts_claude_exe_case_insensitively() {
        let lookup = FakeAncestry::chain(&[(1, 99, "Claude.EXE")]);
        assert_eq!(claude_parent_pid(1, &lookup).unwrap(), 99);
    }

    #[test]
    fn walks_past_shell_layers_to_find_claude() {
        // The PM's own observed chain, exactly: hook(1) <- bash(10) <- bash(20) <- claude(99).
        let lookup =
            FakeAncestry::chain(&[(1, 10, "bash"), (10, 20, "bash"), (20, 99, "claude.exe")]);
        assert_eq!(claude_parent_pid(1, &lookup).unwrap(), 99);
    }

    #[test]
    fn walks_past_a_single_powershell_layer_too() {
        let lookup = FakeAncestry::chain(&[(1, 10, "powershell.exe"), (10, 99, "claude")]);
        assert_eq!(claude_parent_pid(1, &lookup).unwrap(), 99);
    }

    #[test]
    fn refuses_a_non_shell_non_claude_ancestor() {
        // The PM's own example: a stranger appearing before claude is found
        // must refuse, never be walked past the way a shell hop is.
        let lookup = FakeAncestry::chain(&[(1, 99, "node")]);
        assert!(matches!(
            claude_parent_pid(1, &lookup),
            Err(PidError::UnexpectedAncestor(_))
        ));
    }

    #[test]
    fn refuses_a_non_shell_ancestor_even_behind_a_real_shell_hop() {
        let lookup = FakeAncestry::chain(&[(1, 10, "bash"), (10, 99, "node")]);
        assert!(matches!(
            claude_parent_pid(1, &lookup),
            Err(PidError::UnexpectedAncestor(_))
        ));
    }

    #[test]
    fn refuses_when_the_hop_limit_is_exceeded() {
        // All shells, never reaching claude within MAX_ANCESTRY_HOPS.
        let lookup = FakeAncestry::chain(&[
            (1, 10, "bash"),
            (10, 20, "bash"),
            (20, 30, "bash"),
            (30, 40, "bash"),
            (40, 50, "bash"),
        ]);
        assert!(matches!(
            claude_parent_pid(1, &lookup),
            Err(PidError::HopLimitExceeded)
        ));
    }

    #[test]
    fn refuses_when_the_parent_cannot_be_found_at_all() {
        let lookup = FakeAncestry::chain(&[]);
        assert!(matches!(
            claude_parent_pid(1, &lookup),
            Err(PidError::ParentNotFound)
        ));
    }

    // -- parse_claude_cli_flags ------------------------------------------------ //
    // PM finding, 2026-09-18 (fourth interactive retest): no hook field
    // carries `remote_control`, and `permission_mode` isn't reliably present
    // either - both are read from the launch command line instead.

    fn strs(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn remote_control_flag_is_detected() {
        let cmdline = strs(&["claude.exe", "--remote-control"]);
        assert_eq!(
            parse_claude_cli_flags(&cmdline),
            ClaudeCliFlags {
                remote_control: true,
                name: None,
                permission_mode: None,
                launch_args: Some(cmdline.clone()),
            }
        );
    }

    #[test]
    fn permission_mode_value_is_extracted() {
        let cmdline = strs(&["claude.exe", "--permission-mode", "prompting"]);
        assert_eq!(
            parse_claude_cli_flags(&cmdline),
            ClaudeCliFlags {
                remote_control: false,
                name: None,
                permission_mode: Some("prompting".to_string()),
                launch_args: Some(cmdline.clone()),
            }
        );
    }

    #[test]
    fn permission_mode_equals_value_form_is_extracted_too() {
        // PM finding, 2026-09-18 (post-merge, PR #61's own gate read): the
        // space-separated form isn't the only one Claude Code's own CLI
        // accepts - `--permission-mode=value` must parse the same way.
        let cmdline = strs(&["claude.exe", "--permission-mode=bypassPermissions"]);
        assert_eq!(
            parse_claude_cli_flags(&cmdline).permission_mode,
            Some("bypassPermissions".to_string())
        );
    }

    #[test]
    fn dangerously_skip_permissions_maps_to_bypass_permissions() {
        let cmdline = strs(&["claude.exe", "--dangerously-skip-permissions"]);
        assert_eq!(
            parse_claude_cli_flags(&cmdline).permission_mode,
            Some("bypassPermissions".to_string())
        );
    }

    #[test]
    fn no_relevant_flags_leaves_both_fields_at_their_defaults() {
        // `--name` became relevant (it is recorded now); `--verbose` is not.
        let cmdline = strs(&["claude.exe", "--verbose"]);
        assert_eq!(
            parse_claude_cli_flags(&cmdline),
            ClaudeCliFlags {
                launch_args: Some(cmdline.clone()),
                ..ClaudeCliFlags::default()
            }
        );
    }

    #[test]
    fn flags_combine() {
        let cmdline = strs(&[
            "claude.exe",
            "--remote-control",
            "--permission-mode",
            "bypassPermissions",
        ]);
        assert_eq!(
            parse_claude_cli_flags(&cmdline),
            ClaudeCliFlags {
                remote_control: true,
                name: None,
                permission_mode: Some("bypassPermissions".to_string()),
                launch_args: Some(cmdline.clone()),
            }
        );
    }

    #[test]
    fn a_trailing_permission_mode_flag_with_no_value_is_ignored_not_a_panic() {
        let cmdline = strs(&["claude.exe", "--permission-mode"]);
        assert_eq!(
            parse_claude_cli_flags(&cmdline),
            ClaudeCliFlags {
                launch_args: Some(cmdline.clone()),
                ..ClaudeCliFlags::default()
            }
        );
    }
}
