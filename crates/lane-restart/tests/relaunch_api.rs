// SPDX-License-Identifier: MIT OR Apache-2.0
//! `relaunch` is a library module (agentlife's extraction note): the argv a
//! relaunch builds and the checks around it are public, and they answer
//! exactly what the private module inside the binary answered before the
//! move. The expected values below were captured from that private module
//! (a temporary test at origin/main 441b961), not worked out by hand.

use lane_restart::relaunch::{
    claude_argv, extra_launch_args, first_unsafe_argument, has_dev_channels_flag,
    host_wrapped_argv, strip_session_identity_env, valid_identifier,
};
use lane_restart::state::LaneState;

fn strs(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

fn state(role: &str) -> LaneState {
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

#[test]
fn the_argv_of_a_plain_relaunch_is_what_it_was() {
    let st = state("overmind");
    assert_eq!(
        claude_argv(
            "overmind",
            &st,
            "read overmind HANDOFF and continue",
            "sonnet"
        ),
        strs(&[
            "claude",
            "read overmind HANDOFF and continue",
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
fn launch_flags_carry_over_through_the_allowlist_only() {
    let mut st = state("overmind");
    st.remote_control = true;
    let launch = strs(&[
        "claude",
        "--dangerously-load-development-channels",
        "server:claude-peers",
        "--name",
        "PM",
        "--bogus-flag",
        "x",
        "--model",
        "opus",
    ]);
    st.launch_args = Some(launch.clone());
    assert_eq!(
        claude_argv("pm", &st, "p", "claude-sonnet-5"),
        strs(&[
            "claude",
            "p",
            "--name",
            "pm",
            "--model",
            "claude-sonnet-5",
            "--permission-mode",
            "prompting",
            "--remote-control",
            "--dangerously-load-development-channels",
            "server:claude-peers",
        ])
    );
    assert_eq!(
        extra_launch_args(&launch),
        (
            strs(&[
                "--dangerously-load-development-channels",
                "server:claude-peers"
            ]),
            strs(&["--bogus-flag"])
        )
    );
    assert!(has_dev_channels_flag(&launch));
    assert!(!has_dev_channels_flag(&strs(&[
        "claude",
        "--remote-control"
    ])));
}

#[test]
fn a_semicolon_in_any_element_is_unsafe_and_a_plain_argv_is_not() {
    assert_eq!(first_unsafe_argument(["a", "b;c", "d"]), Some("b;c"));
    assert_eq!(first_unsafe_argument(["a", "b", "c"]), None);
}

#[test]
fn the_host_wraps_the_argv_for_its_role() {
    let hosted = host_wrapped_argv("overmind", &strs(&["claude", "x"]));
    // element 0 is this executable's own path; the rest is fixed
    assert_eq!(
        hosted[1..],
        strs(&["host", "--role", "overmind", "--", "claude", "x"])[..]
    );
}

#[test]
fn a_role_must_be_a_plain_identifier() {
    assert!(valid_identifier("overmind"));
    assert!(!valid_identifier("a;b"));
    assert!(!valid_identifier(""));
}

/// The session-identity strip is what keeps a relaunched lane from believing
/// it is a child of the session that asked for the restart.
#[test]
fn the_session_identity_variables_are_removed_from_a_launch() {
    let mut cmd = std::process::Command::new("never-run");
    for v in lane_state::claude_proc::SESSION_IDENTITY_ENV_VARS {
        cmd.env(v, "1");
    }
    cmd.env("CLAUDE_EFFORT", "keep");
    strip_session_identity_env(&mut cmd);
    for v in lane_state::claude_proc::SESSION_IDENTITY_ENV_VARS {
        let kept = cmd
            .get_envs()
            .find(|(k, _)| k == v)
            .and_then(|(_, val)| val);
        assert_eq!(kept, None, "{v} still set");
    }
    let effort = cmd
        .get_envs()
        .find(|(k, _)| *k == "CLAUDE_EFFORT")
        .and_then(|(_, val)| val);
    assert_eq!(effort, Some(std::ffi::OsStr::new("keep")));
}

/// Everything agentlife's note lists is reachable: the two that spawn real
/// processes are only referenced, never called.
#[test]
fn the_launch_and_liveness_entry_points_are_public() {
    let _ = lane_restart::relaunch::spawn_relaunch;
    let _ = lane_restart::relaunch::wait_for_relaunch_liveness;
    let _ = lane_restart::relaunch::kill_and_relaunch;
    let _ = lane_restart::relaunch::describe_dry_run;
}
