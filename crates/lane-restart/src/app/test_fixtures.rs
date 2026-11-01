// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared by the in-crate tests of the commands behind the `Ctx` seam: a state directory with a
//! LIVE lane, and the fake process facts that call it live.

use crate::launch::LaunchSpec;
use crate::role::Role;
use crate::spec::{build, write_atomic, SpecInputs, SpecState, WrittenBy};
use crate::testing::{log, state_at, RecordingFacts};
use chrono::TimeZone;
use std::path::{Path, PathBuf};

/// A tempdir holding a state directory with one LIVE lane `role` (recorded pid 55 started at 100,
/// which [`live_facts`] calls alive) that has a complete spec and a HANDOFF with the given text.
/// Returns `(state_dir, lane_dir)`.
pub(super) fn fixture(root: &Path, role: &str, handoff: &str) -> (PathBuf, PathBuf) {
    let state = root.join("state");
    let lane = root.join(format!("lane-{role}"));
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&lane).unwrap();
    std::fs::write(lane.join("HANDOFF.md"), handoff).unwrap();
    let mut st = state_at(role, "s", "Stop");
    st.pid = 55;
    st.pid_start_secs = Some(100);
    st.cwd = lane.to_string_lossy().into_owned();
    std::fs::write(
        state.join(format!("{role}.json")),
        serde_json::to_string(&st).unwrap(),
    )
    .unwrap();
    let launch = LaunchSpec {
        wsl: None,
        role: Role::new(role).unwrap(),
        name: Role::new(role).unwrap(),
        cwd: lane.to_string_lossy().into_owned(),
        permission_mode: Some("auto".into()),
        remote_control: false,
        launch_args: Some(
            [
                "claude.exe",
                "prompt",
                "--name",
                role,
                "--model",
                "sonnet",
                "--dangerously-load-development-channels",
                "server:claude-peers",
            ]
            .map(String::from)
            .to_vec(),
        ),
    };
    let spec = build(&SpecInputs {
        launch: &launch,
        model: "sonnet",
        session_id: Some("sess-1"),
        pid_start_secs: Some(100),
        state: SpecState::Running,
        written_by: WrittenBy::Restart,
        now: chrono::Utc
            .with_ymd_and_hms(2026, 10, 11, 12, 0, 0)
            .unwrap(),
    })
    .unwrap();
    write_atomic(&state, &spec).unwrap();
    (state, lane)
}

/// Process facts that say pid 55 (started at 100) is the live `claude`.
pub(super) fn live_facts() -> RecordingFacts {
    let mut f = RecordingFacts::new(&log());
    f.live_pid = Some((55, 100));
    f
}

/// Rewrites `role`'s lane state so its last hook write was `secs` before the fake facts' clock
/// (`RecordingFacts::now`, fixed), which is what `quiet_secs` is measured against.
pub(super) fn age_lane_state(state: &Path, role: &str, secs: i64) {
    let path = state.join(format!("{role}.json"));
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let now = RecordingFacts::new(&log()).now;
    v["updated_at"] =
        serde_json::Value::String((now - chrono::Duration::seconds(secs)).to_rfc3339());
    std::fs::write(&path, v.to_string()).unwrap();
}
