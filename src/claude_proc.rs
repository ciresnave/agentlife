// SPDX-License-Identifier: MIT OR Apache-2.0
//! **TEMPORARY COPY** of a few items from OverMind's `lane-restart` library.
//!
//! * **Origin:** <https://github.com/ciresnave/OverMind>, `crates/lane-restart/src/lane_state_writer.rs`
//!   and `crates/lane-restart/src/paths.rs`, commit `c3935babb0ae90b7a75f84eaaf58506cb782bad3`
//!   (`c3935ba`). Each copied item's body is byte-identical at OverMind `main` `8fbd1db` and at the head
//!   of the open speed-up PR #112 (`bcc5d51`), checked by hashing each item at each ref
//!   (2026-10-07): that PR changes only `RealParentProcess` and adds new helpers, none of them here.
//! * **Authorship and licence:** the same owner and the same licence as this crate, `MIT OR
//!   Apache-2.0`; OverMind's doc comments are kept verbatim as credit and as the record of why each
//!   rule exists. Provenance rule: `C:/Projects/CIRESNAVE-EXPECTATIONS.md` §6.4a (PM sign-off,
//!   2026-10-07).
//! * **Why a copy:** `lane-restart` is not on crates.io (the crates.io API answers 404, with
//!   `sysinfo` as a 200 control, 2026-10-07), and the portfolio rule (`CLAUDE.md` §9 "Sources",
//!   `CIRESNAVE-EXPECTATIONS.md` §6.8) forbids a new `git =` dependency. **Blocker:** OverMind extracts
//!   a small crate and CireSnave clears its publish. **Owner:** the OverMind lane. When it is
//!   published, this file is **deleted** and `hook.rs` depends on the crate. Recorded in
//!   `docs/MILESTONES.md` and `docs/COPIED-FROM-OVERMIND.md`.
//! * **What is deliberately NOT copied:** `RealParentProcess` (the slow per-hop scan; `hook.rs` has its
//!   own one-snapshot source that implements [`ParentProcess`]), `LaneState`, the lock, the state
//!   writer. Everything here is pure logic over injected data.
//!
//! Every place this file differs from the original is marked `ADAPTED`, and
//! `docs/COPIED-FROM-OVERMIND.md` lists them all.

use serde::Deserialize;

/// What [`recorded_cwd`] needs to know about an earlier record of the same lane: only the two fields
/// the copied body reads. (ADAPTED: stands in for OverMind's `LaneState`.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownLaunch {
    pub pid: u32,
    pub cwd: String,
}

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
    // ADAPTED: made `pub` (it was private to OverMind's module; the hook calls it from outside).
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
fn transcript_project_dir(transcript_path: &str) -> Option<&str> {
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
// ADAPTED: `existing` was `Option<&LaneState>`; `LaneState` is OverMind's state-file type, which agentlife
// does not have. `KnownLaunch` carries the only two fields the body reads (`pid`, `cwd`), so the
// body below is unchanged.
pub fn recorded_cwd(existing: Option<&KnownLaunch>, input: &HookInput, pid: u32) -> String {
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
                .find(|c| project_dir_name(c).eq_ignore_ascii_case(dir))
        });
    by_transcript.unwrap_or(&candidates[0]).to_string()
}

/// The directory name Claude Code files a session's transcript under,
/// `~/.claude/projects/<this>/`. ⚠️ MATCHES sessions.md: "your working
/// directory path with non-alphanumeric characters replaced by -". Not a
/// guess - quoted directly from the verified documentation this spec cites.
pub fn project_dir_name(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ⚠️ THE REAL-WORLD FAILURE, REPRODUCED AS A PARSE, NOT JUST A STRUCT
    /// LITERAL: a struct built by hand in Rust can't prove the JSON parser
    /// itself tolerates a missing field the way the code above assumes.
    #[test]
    fn a_real_session_start_payload_missing_permission_mode_parses_cleanly() {
        let json = r#"{
            "hook_event_name": "SessionStart",
            "session_id": "abc-123",
            "cwd": "C:/Projects/OverMind"
        }"#;
        let parsed: HookInput = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.permission_mode, None);
        assert_eq!(parsed.model, None);
        assert_eq!(parsed.hook_event_name, "SessionStart");
    }

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

    // -- parse_claude_cli_flags (OverMind's tests, verbatim) ------------------------------- //

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

    // -- agentlife's own additions: pin what the copied code does that OverMind's tests above do
    // not (OverMind records `name` from `-n` / `--name` / `--name=`; its tests for that live in its
    // `main.rs` relaunch module, which is not copied).

    #[test]
    fn name_is_read_from_every_spelling_and_a_trailing_one_is_ignored() {
        for (args, want) in [
            (&["claude.exe", "-n", "PM"][..], Some("PM")),
            (&["claude.exe", "--name", "synapse"][..], Some("synapse")),
            (&["claude.exe", "--name=overmind"][..], Some("overmind")),
            (&["claude.exe", "-n"][..], None),
            (&["claude.exe", "--verbose"][..], None),
        ] {
            let cmdline = strs(args);
            assert_eq!(
                parse_claude_cli_flags(&cmdline).name.as_deref(),
                want,
                "{args:?}"
            );
        }
    }

    #[test]
    fn model_field_accepts_a_plain_string_or_an_object_with_an_id() {
        let plain: HookInput = serde_json::from_str(
            r#"{"hook_event_name":"SessionStart","session_id":"s","cwd":"C:/x","model":"claude-sonnet-5"}"#,
        )
        .unwrap();
        assert_eq!(plain.model.unwrap().into_string(), "claude-sonnet-5");
        let obj: HookInput = serde_json::from_str(
            r#"{"hook_event_name":"SessionStart","session_id":"s","cwd":"C:/x","model":{"id":"claude-opus-5-5"}}"#,
        )
        .unwrap();
        assert_eq!(obj.model.unwrap().into_string(), "claude-opus-5-5");
    }

    // -- recorded_cwd. OverMind's tests for it go through its state writer (`apply_event`); these
    // are the same scenarios and the same expected answers, against the copied function itself.

    fn input_at(cwd: &str, transcript: Option<&str>) -> HookInput {
        HookInput {
            hook_event_name: "SessionStart".to_string(),
            session_id: "s-123".to_string(),
            cwd: cwd.to_string(),
            permission_mode: None,
            model: None,
            transcript_path: transcript.map(str::to_string),
        }
    }

    fn known(cwd: &str, pid: u32) -> KnownLaunch {
        KnownLaunch {
            pid,
            cwd: cwd.to_string(),
        }
    }

    #[test]
    fn a_compaction_from_the_same_process_keeps_the_recorded_launch_directory() {
        let got = recorded_cwd(
            Some(&known("C:/Projects", 42)),
            &input_at("C:/Projects/fuel", None),
            42,
        );
        assert_eq!(
            got, "C:/Projects",
            "the same process's home, not where it has wandered"
        );
    }

    #[test]
    fn a_new_process_takes_its_own_directory() {
        let got = recorded_cwd(
            Some(&known("C:/Projects", 42)),
            &input_at("C:/Projects/OverMind", None),
            77,
        );
        assert_eq!(got, "C:/Projects/OverMind");
        assert_eq!(
            recorded_cwd(None, &input_at("C:/Projects/x", None), 1),
            "C:/Projects/x"
        );
    }

    #[test]
    fn the_directory_matching_the_transcript_path_wins_over_a_wrong_recorded_one() {
        let t = "C:/Users/u/.claude/projects/C--Projects/s-123.jsonl";
        let got = recorded_cwd(
            Some(&known("C:/Projects/fuel", 42)),
            &input_at("C:/Projects", Some(t)),
            42,
        );
        assert_eq!(got, "C:/Projects");
    }

    #[test]
    fn a_backslashed_transcript_path_is_read_the_same_way() {
        let t = r"C:\Users\u\.claude\projects\C--Projects\s-123.jsonl";
        let got = recorded_cwd(
            Some(&known(r"C:\Projects\fuel", 42)),
            &input_at(r"C:\Projects", Some(t)),
            42,
        );
        assert_eq!(got, r"C:\Projects");
    }

    #[test]
    fn with_no_candidate_matching_the_transcript_the_same_process_rule_stands() {
        let t = "C:/Users/u/.claude/projects/C--Projects/s-123.jsonl";
        let got = recorded_cwd(
            Some(&known("C:/Projects/fuel", 42)),
            &input_at("C:/Projects/coderipper", Some(t)),
            42,
        );
        assert_eq!(got, "C:/Projects/fuel");
    }

    #[test]
    fn the_project_directory_name_is_the_cwd_with_non_alphanumerics_replaced_by_dashes() {
        assert_eq!(
            project_dir_name("C:\\Projects\\agentlife"),
            "C--Projects-agentlife"
        );
        assert_eq!(project_dir_name("C:/Projects"), "C--Projects");
    }
}
