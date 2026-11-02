// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared by the in-crate tests of the commands behind the `Ctx` seam: a state directory with a
//! LIVE lane, and the fake process facts that call it live.

use crate::launch::{LaunchSpec, SpawnFn};
use crate::relaunch::RelaunchError;
use crate::role::Role;
use crate::spec::{build, write_atomic, SpecInputs, SpecState, WrittenBy};
use crate::testing::{log, state_at, RecordingFacts};
use chrono::TimeZone;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

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

/// This test process's parent is the live `claude` (pid 55), as in a lane's own shell.
pub(super) struct UnderClaude;
impl crate::lane_state_writer::ParentProcess for UnderClaude {
    fn parent_of(&self, pid: u32) -> Option<(u32, String)> {
        (pid == std::process::id()).then(|| (55, "claude.exe".to_string()))
    }
    fn cmdline_of(&self, _: u32) -> Option<Vec<String>> {
        None
    }
}

/// What a live lane's process looks like to the target checks: its cwd is the lane directory, it has
/// no shell below it, and its transcript is recent. Everything else is the recording fake.
pub(super) struct LiveLaneFacts {
    pub(super) inner: RecordingFacts,
    pub(super) lane: PathBuf,
}
impl crate::facts::SystemFacts for LiveLaneFacts {
    fn is_alive_claude_process(&self, pid: u32) -> bool {
        self.inner.is_alive_claude_process(pid)
    }
    fn cwd_of(&self, _: u32) -> Option<PathBuf> {
        Some(self.lane.clone())
    }
    fn has_live_shell_descendant(&self, _: u32) -> Result<bool, crate::facts::ShellCheckError> {
        Ok(false)
    }
    fn transcript_is_recent(&self, _: &str, _: &str, _: std::time::Duration) -> bool {
        true
    }
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.inner.now()
    }
    fn process_identity(&self, pid: u32) -> Option<crate::facts::ProcessIdentity> {
        self.inner.process_identity(pid)
    }
    fn kill_verified(
        &self,
        pid: u32,
        expected: &crate::facts::ProcessIdentity,
    ) -> Result<(), crate::facts::KillError> {
        self.inner.kill_verified(pid, expected)
    }
    fn find_claude_process_in(&self, cwd: &str, after: u64) -> Option<u32> {
        self.inner.find_claude_process_in(cwd, after)
    }
    fn process_table(&self) -> Result<Vec<crate::facts::ProcEntry>, crate::facts::ShellCheckError> {
        self.inner.process_table()
    }
}

/// The HANDOFF was written long before the lane's idle claim (the fake clock is in 2026-10), so the
/// "it kept working after claiming it had stopped" check passes.
pub(super) fn settle_handoff(lane: &Path) {
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(lane.join("HANDOFF.md"))
        .unwrap();
    f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000))
        .unwrap();
}

/// Counts spawn attempts and fails each one, so no window is ever opened.
pub(super) fn failing_spawn(calls: &Rc<RefCell<u32>>) -> Rc<SpawnFn> {
    let calls = Rc::clone(calls);
    Rc::new(move |_, _| {
        *calls.borrow_mut() += 1;
        Err(RelaunchError::Spawn("no window in a test".into()))
    })
}

/// A spawner that stands in for the launched session: it records the attempt and writes the lane's
/// state file as a new session that has processed a prompt, which is what the wait looks for.
pub(super) fn coming_up_spawn(state: &Path, calls: &Rc<RefCell<u32>>) -> Rc<SpawnFn> {
    let path = state.join("alpha.json");
    let calls = Rc::clone(calls);
    Rc::new(move |_, _| {
        *calls.borrow_mut() += 1;
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v["session_id"] = "s-new".into();
        v["updated_by_event"] = "UserPromptSubmit".into();
        std::fs::write(&path, v.to_string()).unwrap();
        Ok(())
    })
}
