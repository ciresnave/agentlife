// SPDX-License-Identifier: MIT OR Apache-2.0
//! The public surface other crates (and agentlife, once this is published)
//! depend on. Pinned here so a later edit cannot change it unseen.

use lane_state::claude_proc::{
    claude_parent_pid, parse_claude_cli_flags, recorded_cwd, transcript_project_dir, HookInput,
    ModelField, PidError, SESSION_IDENTITY_ENV_VARS,
};
use lane_state::paths::project_dir_name;
use lane_state::state::LaneState;

/// The list is ten names in this order (agentlife's former copy was hashed that way); the relaunch
/// must strip exactly these and no user config var.
#[test]
fn the_session_identity_env_vars_are_the_ten_agentlife_copied() {
    assert_eq!(
        SESSION_IDENTITY_ENV_VARS,
        &[
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
        ]
    );
    // control: a user's own persistent config vars are not on it
    for kept in [
        "CLAUDE_EFFORT",
        "CLAUDE_CODE_USE_POWERSHELL_TOOL",
        "CLAUDE_CODE_EXECPATH",
    ] {
        assert!(!SESSION_IDENTITY_ENV_VARS.contains(&kept), "{kept}");
    }
}

/// Everything agentlife copied is reachable by name, with the one visibility
/// change it needed (`ModelField::into_string`) and `transcript_project_dir`,
/// which `recorded_cwd`'s callers outside the module also need.
#[test]
fn every_item_agentlife_copied_is_public() {
    let model: ModelField = serde_json::from_str(r#"{"id":"sonnet"}"#).unwrap();
    assert_eq!(model.into_string(), "sonnet");
    assert_eq!(
        transcript_project_dir("C:/u/.claude/projects/C--Projects-x/abc.jsonl"),
        Some("C--Projects-x")
    );
    assert_eq!(project_dir_name("C:/Projects/x"), "C--Projects-x");
    let flags = parse_claude_cli_flags(&["claude".into(), "-n".into(), "PM".into()]);
    assert_eq!(flags.name.as_deref(), Some("PM"));
    let input: HookInput = serde_json::from_str(
        r#"{"hook_event_name":"SessionStart","session_id":"s","cwd":"C:/Projects/x"}"#,
    )
    .unwrap();
    assert_eq!(recorded_cwd(None::<&LaneState>, &input, 1), "C:/Projects/x");
    assert!(matches!(
        claude_parent_pid(1, &NoParent),
        Err(PidError::ParentNotFound)
    ));
}

struct NoParent;
impl lane_state::claude_proc::ParentProcess for NoParent {
    fn parent_of(&self, _: u32) -> Option<(u32, String)> {
        None
    }
    fn cmdline_of(&self, _: u32) -> Option<Vec<String>> {
        None
    }
}
