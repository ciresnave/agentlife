// SPDX-License-Identifier: MIT OR Apache-2.0

//! Launching ONE lane, from a description of it rather than from a state
//! file (ARCHITECTURE-LAYERS-SPEC.md section 8b; agentlife's extraction note,
//! seam 1). A restore of a lane that is already dead has a roster entry, not a
//! `LaneState`: `LaunchSpec` is what a launch actually reads.

use crate::facts::SystemFacts;
use crate::permission_mode::PermissionMode;
use crate::relaunch::{
    extra_launch_args, has_dev_channels_flag, policy_default_model, resolve_launch_model,
    spawn_launch, HostedArgv, OpusPermit, RelaunchError, RelaunchOutcome, StateReader,
};
use crate::role::Role;
use crate::state::LaneState;
use crate::wsl::{self, WslTarget};
use std::time::Duration;

/// What a launch needs to know about the lane it starts. Built from a state
/// file for a restart (`From<&LaneState>`), or by hand from a roster entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    pub role: Role,
    /// The `--name` the session is started with: the lane's recorded name,
    /// else its role.
    pub name: Role,
    pub cwd: String,
    pub permission_mode: Option<PermissionMode>,
    pub remote_control: bool,
    /// The flags of the ORIGINAL launch; only the allowlisted ones carry over.
    pub launch_args: Option<Vec<String>>,
    /// `Some` for a lane that runs in a WSL2 distro (docs/LANE-RESTART-WSL.md): `cwd` is then a
    /// POSIX path inside the distro and the launch is `wsl.exe ...`. `None` is a Windows lane.
    pub wsl: Option<WslTarget>,
}

/// A state file's role or name is whatever a hook wrote, so the conversion is where it is checked:
/// holding a `LaunchSpec` is proof its role and name are plain identifiers, and a restart whose
/// state carries anything else is refused here, before anything is written or killed.
impl TryFrom<&LaneState> for LaunchSpec {
    type Error = RelaunchError;

    fn try_from(state: &LaneState) -> Result<Self, RelaunchError> {
        let role = Role::new(state.role.clone())
            .map_err(|e| RelaunchError::InvalidIdentifier(format!("role {:?}", e.0)))?;
        let name = match &state.name {
            Some(n) => Role::new(n.clone())
                .map_err(|e| RelaunchError::InvalidIdentifier(format!("name {:?}", e.0)))?,
            None => role.clone(),
        };
        Ok(LaunchSpec {
            wsl: None,
            role,
            name,
            cwd: state.cwd.clone(),
            permission_mode: state.permission_mode.clone(),
            remote_control: state.remote_control,
            launch_args: state.launch_args.clone(),
        })
    }
}

/// How a launch is spawned: given the spec and the argv, start the lane's
/// window. The real one is `relaunch::spawn_launch`; tests pass a recorder.
pub type SpawnFn = dyn Fn(&LaunchSpec, &HostedArgv) -> Result<(), RelaunchError>;

/// A launch that passed every check and is ready to spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedLaunch {
    /// The checked command line; derefs to the claude argv.
    pub argv: HostedArgv,
    /// Whether the argv about to be launched carries the development-channels
    /// flag (the startup dialog a human must confirm).
    pub carries_dev_channels_flag: bool,
}

/// The argv of a launch. The prompt goes FIRST, right after `claude`, and the
/// variadic flags carried over from the original launch are always LAST, so
/// nothing can follow a variadic flag's values (see `relaunch::claude_argv`,
/// which builds the same thing from a state file).
pub fn launch_argv(spec: &LaunchSpec, prompt: &str, model: &str) -> Vec<String> {
    let mut argv = vec!["claude".to_string(), prompt.to_string()];
    argv.push("--name".to_string());
    argv.push(spec.name.to_string());
    argv.push("--model".to_string());
    argv.push(model.to_string());
    if let Some(permission_mode) = &spec.permission_mode {
        argv.push("--permission-mode".to_string());
        argv.push(permission_mode.as_str().to_string());
    }
    if spec.remote_control {
        argv.push("--remote-control".to_string());
    }
    if let Some(launch_args) = &spec.launch_args {
        let (extra, dropped_unknown) = extra_launch_args(launch_args);
        argv.extend(extra);
        if !dropped_unknown.is_empty() {
            eprintln!(
                "lane-restart: dropping unrecognised launch flag(s), not carrying them \
                 over to the relaunch: {}",
                dropped_unknown.join(", ")
            );
        }
    }
    argv
}

/// The first prompt of every launched lane. The second clause is the re-arm half of "waits that
/// cannot be forgotten" (`waits.rs`): a session timer dies with its session, so the next session
/// must set it again from the HANDOFF.
pub fn startup_prompt(role: &str) -> String {
    format!("read {role} HANDOFF and continue, and arm every WAIT in it (lane-restart waits)")
}

/// Every check a launch must pass BEFORE anything is killed or spawned: a
/// plain role and name, a model that is not Opus (unless overridden), and no
/// `;` in the working directory or any argv element (`wt.exe` reads it as a
/// command separator). Returns the argv about to be launched.
pub fn prepare_launch(
    spec: &LaunchSpec,
    policy_default: &str,
    permit: Option<&OpusPermit>,
) -> Result<PreparedLaunch, RelaunchError> {
    let prompt = startup_prompt(spec.role.as_str());
    let model = resolve_launch_model(spec.launch_args.as_ref(), policy_default, permit)?;
    let argv = launch_argv(spec, &prompt, model.as_str());
    // Checked as the FINAL command line (cwd + host-wrapped argv, the `lane-restart.exe` path
    // included): `HostedArgv` is the only thing a spawn accepts.
    let argv = match &spec.wsl {
        None => HostedArgv::new(&spec.cwd, spec.role.as_str(), argv)?,
        Some(target) => {
            let command = wsl::build_command(target, &spec.cwd, &argv)
                .map_err(|e| RelaunchError::Wsl(e.to_string()))?;
            HostedArgv::new_wsl(&wsl::host_start_dir(), spec.role.as_str(), argv, command)?
        }
    };
    // PM finding, 2026-09-19: judge the argv ABOUT TO BE LAUNCHED (it already
    // went through the allowlist), never the original launch flags.
    let carries_dev_channels_flag = has_dev_channels_flag(&argv);
    Ok(PreparedLaunch {
        argv,
        carries_dev_channels_flag,
    })
}

/// How long a launch is given. `Default` is what lane-restart has always
/// used; a batch restore passes its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LivenessTiming {
    /// How long without real progress before the process is checked.
    pub progress_timeout: Duration,
    /// How long a launched process is given to APPEAR at all. A claude that has not been seen yet
    /// at `progress_timeout` is waited for until this long; once seen, its disappearance is still
    /// judged at `progress_timeout`. (2026-10-18: lanes whose claude appeared 8-28 s after the
    /// spawn were declared dead at 20 s and left `stopping`, though they came up.)
    pub appear_timeout: Duration,
    /// How long in all before giving up (or reporting the dialog as final).
    pub total_timeout: Duration,
    pub poll_interval: Duration,
}

impl Default for LivenessTiming {
    fn default() -> Self {
        LivenessTiming {
            progress_timeout: Duration::from_secs(20),
            appear_timeout: Duration::from_secs(120),
            // "Configurable" per the PM's own spec; now it is, as a parameter.
            total_timeout: Duration::from_secs(15 * 60),
            poll_interval: Duration::from_secs(1),
        }
    }
}

/// Waits for a launched lane to show real progress: its own state file shows a
/// session that is not `previous_session` (when there is one) and whose last
/// event is past `SessionStart`. Three outcomes, as `relaunch`'s wait always had:
/// `Relaunched`; `AwaitingConfirmation` (alive, no progress, and the launch
/// carries the development-channels flag: `on_awaiting` fires once, polling
/// goes on to the total timeout); or `Err(SessionNeverProcessedPrompt)`.
/// `previous_session` is `None` for a lane that had no session to replace (a
/// restore); `launched_after_secs` is the earliest start time of the new
/// process. `sleep` and `on_awaiting` are injected so the whole protocol is
/// testable without a real wait.
#[allow(clippy::too_many_arguments)]
pub fn wait_for_liveness(
    facts: &dyn SystemFacts,
    state_reader: &dyn StateReader,
    cwd: &str,
    role: &str,
    previous_session: Option<&str>,
    launched_after_secs: u64,
    carries_dev_channels_flag: bool,
    timing: &LivenessTiming,
    sleep: &mut dyn FnMut(Duration),
    on_awaiting: &mut dyn FnMut(),
) -> Result<RelaunchOutcome, RelaunchError> {
    let mut waited = std::time::Duration::ZERO;
    let mut reported_awaiting = false;
    let mut seen_alive = false;
    // The spawn has just returned; the margin covers the stub wait and the clock's step.
    let since = std::time::SystemTime::now() - Duration::from_secs(10);
    let failure = |seen_alive: bool| {
        if seen_alive {
            return RelaunchError::SessionNeverProcessedPrompt;
        }
        match state_reader.host_started(role, since) {
            Some(false) => RelaunchError::HostNeverStarted,
            Some(true) => RelaunchError::ClaudeNeverStarted,
            None => RelaunchError::SessionNeverProcessedPrompt,
        }
    };
    loop {
        if let Some(state) = state_reader.read(role) {
            if previous_session.is_none_or(|old| state.session_id != old)
                && state.updated_by_event != "SessionStart"
            {
                return Ok(RelaunchOutcome::Relaunched);
            }
        }
        if waited >= timing.progress_timeout {
            let alive = facts
                .find_claude_process_in(cwd, launched_after_secs)
                .is_some();
            if alive {
                seen_alive = true;
            } else if seen_alive || waited >= timing.appear_timeout {
                return Err(failure(seen_alive));
            }
            if alive && !reported_awaiting {
                if !carries_dev_channels_flag {
                    // Alive, no progress, and nothing known to explain
                    // the stall - not the dialog case. Keep polling to
                    // timing.total_timeout anyway (a slow session is still a
                    // real possibility), but never report a confirmation
                    // that has no evidence behind it.
                } else {
                    on_awaiting();
                    reported_awaiting = true;
                }
            }
        }
        if waited >= timing.total_timeout {
            return if reported_awaiting {
                Ok(RelaunchOutcome::AwaitingConfirmation)
            } else {
                Err(failure(seen_alive))
            };
        }
        sleep(timing.poll_interval);
        waited += timing.poll_interval;
    }
}

/// Launches a lane and waits until it is working: NO kill. This is
/// `lane-start` (spec 8b; agentlife's seam 3). Every check runs before
/// anything is spawned, and the clock is read after the checks, so the new
/// process is recognised by starting after (now - the clock margin).
/// `previous_session` is the session being replaced, if any; a restore has
/// none. The policy default model, the Opus override, the spawn and the sleep
/// are parameters so the order is testable; `launch_and_wait` fills them in.
#[allow(clippy::too_many_arguments)]
pub fn launch_and_wait_with(
    facts: &dyn SystemFacts,
    state_reader: &dyn StateReader,
    spec: &LaunchSpec,
    previous_session: Option<&str>,
    timing: &LivenessTiming,
    policy_default: &str,
    permit: Option<&OpusPermit>,
    spawn: &SpawnFn,
    sleep: &mut dyn FnMut(Duration),
    on_awaiting: &mut dyn FnMut(),
) -> Result<RelaunchOutcome, RelaunchError> {
    let PreparedLaunch {
        argv,
        carries_dev_channels_flag,
    } = prepare_launch(spec, policy_default, permit)?;
    let launched_after_secs = crate::stop::launched_after(facts.now());
    spawn(spec, &argv)?;
    wait_for_liveness(
        facts,
        state_reader,
        &spec.cwd,
        spec.role.as_str(),
        previous_session,
        launched_after_secs,
        carries_dev_channels_flag,
        timing,
        sleep,
        on_awaiting,
    )
}

/// `launch_and_wait_with` on this machine: the real spawn (`wt.exe`, else
/// `conhost.exe`), the real sleep, the portfolio policy model and the Opus
/// override from the environment. NOT unit tested, like `spawn_launch`: it
/// starts a real window.
pub fn launch_and_wait(
    facts: &dyn SystemFacts,
    state_reader: &dyn StateReader,
    spec: &LaunchSpec,
    previous_session: Option<&str>,
    timing: &LivenessTiming,
    on_awaiting: &mut dyn FnMut(),
) -> Result<RelaunchOutcome, RelaunchError> {
    launch_and_wait_with(
        facts,
        state_reader,
        spec,
        previous_session,
        timing,
        &policy_default_model(&crate::state_dir()),
        OpusPermit::from_env().as_ref(),
        &spawn_launch,
        &mut std::thread::sleep,
        on_awaiting,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relaunch::claude_argv;

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
            permission_mode: Some("prompting".into()),
            remote_control: false,
            busy: false,
            subagents_running: 0,
            no_background_shells: Some(true),
            launch_args: None,
            updated_at: chrono::Utc::now(),
            updated_by_event: "Stop".to_string(),
        }
    }

    fn spec(role: &str) -> LaunchSpec {
        LaunchSpec::try_from(&state(role)).unwrap()
    }

    #[test]
    fn a_spec_from_a_state_copies_what_a_launch_reads_and_names_the_role_by_default() {
        let mut st = state("overmind");
        st.remote_control = true;
        st.launch_args = Some(strs(&["claude", "--remote-control"]));
        let s = LaunchSpec::try_from(&st).unwrap();
        assert_eq!(s.role.as_str(), "overmind");
        assert_eq!(s.name.as_str(), "overmind");
        assert_eq!(s.cwd, "C:/x");
        assert_eq!(
            s.permission_mode.as_ref().map(|m| m.as_str()),
            Some("prompting")
        );
        assert!(s.remote_control);
        assert_eq!(s.launch_args, st.launch_args);
        // control: a recorded name wins over the role
        st.name = Some("PM".into());
        assert_eq!(LaunchSpec::try_from(&st).unwrap().name.as_str(), "PM");
    }

    /// The restart path and the new spec path build the SAME argv, for every
    /// shape of state, so a restore and a restart can never drift apart.
    #[test]
    fn a_spec_builds_the_same_argv_as_the_state_it_came_from() {
        let mut plain = state("overmind");
        plain.permission_mode = None;
        let mut remote = state("pm");
        remote.remote_control = true;
        remote.launch_args = Some(strs(&[
            "claude",
            "--dangerously-load-development-channels",
            "server:claude-peers",
            "--bogus-flag",
            "x",
        ]));
        let mut named = state("overmind");
        named.name = Some("Over".into());
        for st in [state("overmind"), plain, remote, named] {
            let spec = LaunchSpec::try_from(&st).unwrap();
            let name = st.name.clone().unwrap_or_else(|| st.role.clone());
            assert_eq!(
                launch_argv(&spec, "p", "sonnet"),
                claude_argv(&name, &st, "p", "sonnet").unwrap(),
                "{}",
                st.role
            );
        }
    }

    /// A roster entry has no state file: the argv comes from the spec alone.
    #[test]
    fn a_hand_built_spec_launches_without_a_state_file() {
        let spec = LaunchSpec {
            wsl: None,
            role: Role::new("fuel").unwrap(),
            name: Role::new("fuel").unwrap(),
            cwd: "C:/Projects/fuel".into(),
            permission_mode: None,
            remote_control: false,
            launch_args: None,
        };
        assert_eq!(
            launch_argv(&spec, "read fuel HANDOFF and continue", "sonnet"),
            strs(&[
                "claude",
                "read fuel HANDOFF and continue",
                "--name",
                "fuel",
                "--model",
                "sonnet",
            ])
        );
    }

    #[test]
    fn a_state_with_an_invalid_role_or_name_makes_no_launch_spec() {
        // A `LaunchSpec` cannot hold either, so the refusal is the conversion's.
        assert!(matches!(
            LaunchSpec::try_from(&state("a&calc")),
            Err(RelaunchError::InvalidIdentifier(_))
        ));
        let mut st = state("overmind");
        st.name = Some("a|calc".into());
        assert!(matches!(
            LaunchSpec::try_from(&st),
            Err(RelaunchError::InvalidIdentifier(_))
        ));
    }

    #[test]
    fn a_launch_is_refused_when_any_element_has_a_semicolon() {
        let mut s = spec("overmind");
        s.cwd = "C:/x;calc".into();
        assert!(matches!(
            prepare_launch(&s, "sonnet", None),
            Err(RelaunchError::UnsafeArgument(_))
        ));
        let mut s = spec("overmind");
        s.permission_mode = Some("a;b".into());
        assert!(matches!(
            prepare_launch(&s, "sonnet", None),
            Err(RelaunchError::UnsafeArgument(_))
        ));
    }

    /// CireSnave, 2026-10-08: "I can't afford Opus."
    #[test]
    fn a_launch_on_opus_is_refused_unless_overridden() {
        assert!(matches!(
            prepare_launch(&spec("overmind"), "claude-opus-5", None),
            Err(RelaunchError::ModelRefused(_))
        ));
        let ok = prepare_launch(
            &spec("overmind"),
            "claude-opus-5",
            Some(&OpusPermit::for_test()),
        )
        .unwrap();
        assert!(ok.argv.contains(&"claude-opus-5".to_string()));
        // an explicit launch --model pin wins over the policy default
        let mut pinned = spec("overmind");
        pinned.launch_args = Some(strs(&["claude", "--model", "haiku"]));
        let p = prepare_launch(&pinned, "claude-opus-5", None).unwrap();
        assert!(p.argv.contains(&"haiku".to_string()));
    }

    #[test]
    fn a_prepared_launch_knows_whether_it_carries_the_dev_channels_flag() {
        let plain = prepare_launch(&spec("overmind"), "sonnet", None).unwrap();
        assert!(!plain.carries_dev_channels_flag);
        assert_eq!(plain.argv[0], "claude");
        let mut s = spec("overmind");
        s.launch_args = Some(strs(&[
            "claude",
            "--dangerously-load-development-channels",
            "server:claude-peers",
        ]));
        let p = prepare_launch(&s, "sonnet", None).unwrap();
        assert!(p.carries_dev_channels_flag);
    }

    // -- LivenessTiming and wait_for_liveness (seam 2) ---------------------------

    struct Reader(Vec<Option<LaneState>>, std::cell::RefCell<usize>);
    impl StateReader for Reader {
        fn read(&self, _role: &str) -> Option<LaneState> {
            let mut calls = self.1.borrow_mut();
            let idx = (*calls).min(self.0.len() - 1);
            *calls += 1;
            self.0[idx].clone()
        }
    }

    fn reader(states: Vec<Option<LaneState>>) -> Reader {
        Reader(states, std::cell::RefCell::new(0))
    }

    fn at(session: &str, event: &str) -> Option<LaneState> {
        let mut st = state("overmind");
        st.session_id = session.into();
        st.updated_by_event = event.into();
        Some(st)
    }

    /// Only `find_claude_process_in` is ever asked.
    struct Alive(bool);
    impl SystemFacts for Alive {
        fn is_alive_claude_process(&self, _: u32) -> bool {
            unreachable!()
        }
        fn cwd_of(&self, _: u32) -> Option<std::path::PathBuf> {
            unreachable!()
        }
        fn has_live_shell_descendant(&self, _: u32) -> Result<bool, crate::facts::ShellCheckError> {
            unreachable!()
        }
        fn transcript_is_recent(&self, _: &str, _: &str, _: Duration) -> bool {
            unreachable!()
        }
        fn now(&self) -> chrono::DateTime<chrono::Utc> {
            unreachable!()
        }
        fn process_identity(&self, _: u32) -> Option<crate::facts::ProcessIdentity> {
            unreachable!()
        }
        fn kill_verified(
            &self,
            _: u32,
            _: &crate::facts::ProcessIdentity,
        ) -> Result<(), crate::facts::KillError> {
            unreachable!()
        }
        fn find_claude_process_in(&self, _: &str, _: u64) -> Option<u32> {
            self.0.then_some(1)
        }
        fn process_table(
            &self,
        ) -> Result<Vec<crate::facts::ProcEntry>, crate::facts::ShellCheckError> {
            unreachable!()
        }
    }

    fn short() -> LivenessTiming {
        LivenessTiming {
            progress_timeout: Duration::from_secs(2),
            appear_timeout: Duration::from_secs(2),
            total_timeout: Duration::from_secs(5),
            poll_interval: Duration::from_secs(1),
        }
    }

    #[test]
    fn the_default_timing_is_what_lane_restart_has_always_used() {
        let t = LivenessTiming::default();
        assert_eq!(t.progress_timeout, Duration::from_secs(20));
        assert_eq!(t.appear_timeout, Duration::from_secs(120));
        assert_eq!(t.total_timeout, Duration::from_secs(15 * 60));
        assert_eq!(t.poll_interval, Duration::from_secs(1));
    }

    /// A batch restore wants a short wait: the parameters really are used.
    #[test]
    fn a_custom_timing_decides_when_the_dialog_is_reported_and_when_to_stop() {
        let r = reader(vec![at("new", "SessionStart")]);
        let mut slept = Vec::new();
        let mut awaiting = 0;
        let out = wait_for_liveness(
            &Alive(true),
            &r,
            "C:/x",
            "overmind",
            Some("old"),
            0,
            true,
            &short(),
            &mut |d| slept.push(d),
            &mut || awaiting += 1,
        );
        assert_eq!(out.unwrap(), RelaunchOutcome::AwaitingConfirmation);
        assert_eq!(slept, vec![Duration::from_secs(1); 5], "5 s in 1 s steps");
        assert_eq!(awaiting, 1, "reported once");
    }

    /// `find_claude_process_in` answers false for the first `n` asks, then true.
    struct AppearsAfter(u32, std::cell::Cell<u32>);
    impl SystemFacts for AppearsAfter {
        fn is_alive_claude_process(&self, _: u32) -> bool {
            unreachable!()
        }
        fn cwd_of(&self, _: u32) -> Option<std::path::PathBuf> {
            unreachable!()
        }
        fn now(&self) -> chrono::DateTime<chrono::Utc> {
            unreachable!()
        }
        fn process_identity(&self, _: u32) -> Option<crate::facts::ProcessIdentity> {
            unreachable!()
        }
        fn kill_verified(
            &self,
            _: u32,
            _: &crate::facts::ProcessIdentity,
        ) -> Result<(), crate::facts::KillError> {
            unreachable!()
        }
        fn find_claude_process_in(&self, _: &str, _: u64) -> Option<u32> {
            let asked = self.1.get();
            self.1.set(asked + 1);
            (asked >= self.0).then_some(1)
        }
        fn has_live_shell_descendant(&self, _: u32) -> Result<bool, crate::facts::ShellCheckError> {
            unreachable!()
        }
        fn transcript_is_recent(&self, _: &str, _: &str, _: Duration) -> bool {
            unreachable!()
        }
        fn process_table(
            &self,
        ) -> Result<Vec<crate::facts::ProcEntry>, crate::facts::ShellCheckError> {
            unreachable!()
        }
    }

    fn wait_with(
        facts: &dyn SystemFacts,
        timing: &LivenessTiming,
    ) -> Result<RelaunchOutcome, RelaunchError> {
        wait_for_liveness(
            facts,
            &reader(vec![at("new", "SessionStart")]),
            "C:/x",
            "overmind",
            Some("old"),
            0,
            true,
            timing,
            &mut |_| {},
            &mut || {},
        )
    }

    /// 2026-10-18: a claude that appeared after the progress timeout was declared dead.
    #[test]
    fn a_process_that_appears_late_is_waited_for_not_declared_dead() {
        let timing = LivenessTiming {
            appear_timeout: Duration::from_secs(4),
            ..short()
        };
        // absent at the 2 s and 3 s checks, present from the third ask
        let late = AppearsAfter(2, std::cell::Cell::new(0));
        assert_eq!(
            wait_with(&late, &timing).unwrap(),
            RelaunchOutcome::AwaitingConfirmation
        );
        // control: never appearing still fails, at the appear timeout
        let never = AppearsAfter(u32::MAX, std::cell::Cell::new(0));
        assert!(matches!(
            wait_with(&never, &timing),
            Err(RelaunchError::SessionNeverProcessedPrompt)
        ));
        assert_eq!(never.1.get(), 3, "asked at 2 s, 3 s and 4 s, then gave up");
    }

    /// A reader that knows whether the host left its boot record.
    struct Booted(Option<bool>);
    impl StateReader for Booted {
        fn read(&self, _role: &str) -> Option<LaneState> {
            at("new", "SessionStart")
        }
        fn host_started(&self, _role: &str, _since: std::time::SystemTime) -> Option<bool> {
            self.0
        }
    }

    fn wait_booted(
        boot: Option<bool>,
        alive: &dyn SystemFacts,
    ) -> Result<RelaunchOutcome, RelaunchError> {
        let timing = LivenessTiming {
            appear_timeout: Duration::from_secs(4),
            ..short()
        };
        wait_for_liveness(
            alive,
            &Booted(boot),
            "C:/x",
            "overmind",
            Some("old"),
            0,
            true,
            &timing,
            &mut |_| {},
            &mut || {},
        )
    }

    /// 2026-10-18: three restarts failed with nothing to say whether the host ever ran.
    #[test]
    fn a_failure_names_the_stage_that_never_happened() {
        let never = AppearsAfter(u32::MAX, std::cell::Cell::new(0));
        assert!(matches!(
            wait_booted(Some(false), &never),
            Err(RelaunchError::HostNeverStarted)
        ));
        assert!(matches!(
            wait_booted(Some(true), &never),
            Err(RelaunchError::ClaudeNeverStarted)
        ));
        // a reader that cannot tell keeps the old, unnamed failure
        assert!(matches!(
            wait_booted(None, &never),
            Err(RelaunchError::SessionNeverProcessedPrompt)
        ));
        for e in [
            RelaunchError::HostNeverStarted,
            RelaunchError::ClaudeNeverStarted,
        ] {
            assert!(e.to_string().starts_with("relaunch FAILED:"), "{e}");
        }
    }

    /// A claude that WAS seen and then died is the session's failure, whatever the host did.
    #[test]
    fn a_session_that_was_seen_keeps_its_own_failure_whatever_the_boot_record_says() {
        let alive = AppearsAfter(0, std::cell::Cell::new(0));
        // seen alive at the progress check, so a later total-timeout failure is not a start failure
        let timing = LivenessTiming {
            appear_timeout: Duration::from_secs(4),
            total_timeout: Duration::from_secs(3),
            ..short()
        };
        let r = wait_for_liveness(
            &alive,
            &Booted(Some(false)),
            "C:/x",
            "overmind",
            Some("old"),
            0,
            false,
            &timing,
            &mut |_| {},
            &mut || {},
        );
        assert!(
            matches!(r, Err(RelaunchError::SessionNeverProcessedPrompt)),
            "{r:?}"
        );
    }

    #[test]
    fn a_process_seen_and_then_gone_fails_at_once() {
        let timing = LivenessTiming {
            appear_timeout: Duration::from_secs(4),
            ..short()
        };
        let asked = std::cell::Cell::new(0u32);
        // alive on the first ask, gone on the second
        struct Facts<'a>(&'a std::cell::Cell<u32>);
        impl SystemFacts for Facts<'_> {
            fn is_alive_claude_process(&self, _: u32) -> bool {
                unreachable!()
            }
            fn cwd_of(&self, _: u32) -> Option<std::path::PathBuf> {
                unreachable!()
            }
            fn now(&self) -> chrono::DateTime<chrono::Utc> {
                unreachable!()
            }
            fn process_identity(&self, _: u32) -> Option<crate::facts::ProcessIdentity> {
                unreachable!()
            }
            fn kill_verified(
                &self,
                _: u32,
                _: &crate::facts::ProcessIdentity,
            ) -> Result<(), crate::facts::KillError> {
                unreachable!()
            }
            fn find_claude_process_in(&self, _: &str, _: u64) -> Option<u32> {
                let n = self.0.get();
                self.0.set(n + 1);
                (n == 0).then_some(1)
            }
            fn has_live_shell_descendant(
                &self,
                _: u32,
            ) -> Result<bool, crate::facts::ShellCheckError> {
                unreachable!()
            }
            fn transcript_is_recent(&self, _: &str, _: &str, _: Duration) -> bool {
                unreachable!()
            }
            fn process_table(
                &self,
            ) -> Result<Vec<crate::facts::ProcEntry>, crate::facts::ShellCheckError> {
                unreachable!()
            }
        }
        assert!(matches!(
            wait_with(&Facts(&asked), &timing),
            Err(RelaunchError::SessionNeverProcessedPrompt)
        ));
        assert_eq!(asked.get(), 2, "gone at the second check, well before 4 s");
    }

    #[test]
    fn a_dead_process_is_found_at_the_custom_progress_timeout() {
        let r = reader(vec![None]);
        let mut slept = 0;
        let out = wait_for_liveness(
            &Alive(false),
            &r,
            "C:/x",
            "overmind",
            Some("old"),
            0,
            false,
            &short(),
            &mut |_| slept += 1,
            &mut || {},
        );
        assert!(matches!(
            out,
            Err(RelaunchError::SessionNeverProcessedPrompt)
        ));
        assert_eq!(slept, 2, "gave up at the 2 s progress timeout, not at 5 s");
    }

    /// A restore has no previous session: any session that got past
    /// `SessionStart` is the fresh one.
    #[test]
    fn with_no_previous_session_any_progress_counts() {
        let none = wait_for_liveness(
            &Alive(true),
            &reader(vec![at("whatever", "UserPromptSubmit")]),
            "C:/x",
            "overmind",
            None,
            0,
            false,
            &short(),
            &mut |_| {},
            &mut || {},
        );
        assert_eq!(none.unwrap(), RelaunchOutcome::Relaunched);
        // control: still only SessionStart is not yet progress
        let mut slept = 0;
        let stuck = wait_for_liveness(
            &Alive(false),
            &reader(vec![at("whatever", "SessionStart")]),
            "C:/x",
            "overmind",
            None,
            0,
            false,
            &short(),
            &mut |_| slept += 1,
            &mut || {},
        );
        assert!(stuck.is_err());
        assert_eq!(slept, 2);
    }

    /// With a previous session, its own later events are not the new session.
    #[test]
    fn the_previous_sessions_own_events_do_not_count() {
        let mut slept = 0;
        let out = wait_for_liveness(
            &Alive(false),
            &reader(vec![at("old", "UserPromptSubmit")]),
            "C:/x",
            "overmind",
            Some("old"),
            0,
            false,
            &short(),
            &mut |_| slept += 1,
            &mut || {},
        );
        assert!(out.is_err());
        assert_eq!(slept, 2);
    }

    // -- launch_and_wait (seam 3): a start with no stop ----------------------

    use crate::testing::{log, state_at, RecordingFacts, ScriptedReader};

    fn start(
        l: &crate::testing::Log,
        facts: &RecordingFacts,
        reader: &ScriptedReader,
        spec: &LaunchSpec,
        previous: Option<&str>,
    ) -> Result<RelaunchOutcome, RelaunchError> {
        let lg = std::rc::Rc::clone(l);
        launch_and_wait_with(
            facts,
            reader,
            spec,
            previous,
            &short(),
            "sonnet",
            None,
            &move |s, argv| {
                lg.borrow_mut()
                    .push(format!("spawn {} {} {}", s.role, s.cwd, argv.join(" ")));
                Ok(())
            },
            &mut |_| {},
            &mut || {},
        )
    }

    fn fuel() -> LaunchSpec {
        LaunchSpec {
            wsl: None,
            role: Role::new("fuel").unwrap(),
            name: Role::new("fuel").unwrap(),
            cwd: "C:/Projects/fuel".into(),
            permission_mode: None,
            remote_control: false,
            launch_args: None,
        }
    }

    /// A start never kills: `kill_verified` would be logged.
    #[test]
    fn a_start_spawns_and_waits_and_kills_nothing() {
        let l = log();
        let facts = RecordingFacts::new(&l);
        let reader =
            ScriptedReader::new(&l, vec![Some(state_at("fuel", "s1", "UserPromptSubmit"))]);
        let out = start(&l, &facts, &reader, &fuel(), None).unwrap();
        assert_eq!(out, RelaunchOutcome::Relaunched);
        let log = l.borrow();
        assert_eq!(
            log[0], "now",
            "the clock is read first, for the earliest start time"
        );
        assert_eq!(
            log[1],
            format!(
                "spawn fuel C:/Projects/fuel claude {} --name fuel --model sonnet",
                startup_prompt("fuel")
            )
        );
        assert!(!log.iter().any(|e| e.starts_with("kill")), "{log:?}");
    }

    /// The new process must have started after (now - the clock margin).
    #[test]
    fn a_start_asks_for_a_process_newer_than_the_clock_minus_the_margin() {
        let l = log();
        let facts = RecordingFacts::new(&l);
        let reader = ScriptedReader::new(&l, vec![Some(state_at("fuel", "s1", "SessionStart"))]);
        let _ = start(&l, &facts, &reader, &fuel(), None);
        let want = format!("poll C:/Projects/fuel after={}", facts.now.timestamp() - 5);
        assert!(l.borrow().contains(&want), "{:?}", l.borrow());
    }

    #[test]
    fn a_start_refuses_a_bad_launch_before_it_spawns_or_even_reads_the_clock() {
        type Mutate = fn(&mut LaunchSpec);
        let cases: Vec<(&str, Mutate)> = vec![("cwd", |s| s.cwd = "C:/x;calc".into())];
        for (what, mutate) in cases {
            let l = log();
            let facts = RecordingFacts::new(&l);
            let reader = ScriptedReader::new(&l, vec![None]);
            let mut s = fuel();
            mutate(&mut s);
            assert!(start(&l, &facts, &reader, &s, None).is_err(), "{what}");
            assert!(l.borrow().is_empty(), "{what}: {:?}", l.borrow());
        }
        // Opus, without the override
        let l = log();
        let facts = RecordingFacts::new(&l);
        let reader = ScriptedReader::new(&l, vec![None]);
        let out = launch_and_wait_with(
            &facts,
            &reader,
            &fuel(),
            None,
            &short(),
            "claude-opus-5",
            None,
            &|_, _| panic!("spawned an Opus launch"),
            &mut |_| {},
            &mut || {},
        );
        assert!(matches!(out, Err(RelaunchError::ModelRefused(_))));
        assert!(l.borrow().is_empty());
    }

    #[test]
    fn a_failed_spawn_is_the_result_and_nothing_is_waited_for() {
        let l = log();
        let facts = RecordingFacts::new(&l);
        let reader = ScriptedReader::new(&l, vec![None]);
        let out = launch_and_wait_with(
            &facts,
            &reader,
            &fuel(),
            None,
            &short(),
            "sonnet",
            None,
            &|_, _| Err(RelaunchError::Spawn("no wt".into())),
            &mut |_| {},
            &mut || {},
        );
        assert!(matches!(out, Err(RelaunchError::Spawn(_))));
        assert!(
            !l.borrow().iter().any(|e| e.starts_with("read")),
            "{:?}",
            l.borrow()
        );
    }

    #[test]
    fn a_start_uses_the_timing_it_is_given() {
        let l = log();
        let mut facts = RecordingFacts::new(&l);
        facts.alive = false;
        let reader = ScriptedReader::new(&l, vec![None]);
        let mut slept = 0;
        let out = launch_and_wait_with(
            &facts,
            &reader,
            &fuel(),
            None,
            &short(),
            "sonnet",
            None,
            &|_, _| Ok(()),
            &mut |_| slept += 1,
            &mut || {},
        );
        assert!(matches!(
            out,
            Err(RelaunchError::SessionNeverProcessedPrompt)
        ));
        assert_eq!(slept, 2, "the 2 s progress timeout of `short()`");
    }

    // ---- launch mode `wsl` (docs/LANE-RESTART-WSL.md section 3) ----

    fn wsl_spec(role: &str) -> LaunchSpec {
        let mut s = spec(role);
        s.cwd = "/home/lane-commysdk/work/commy-sdk-rust".to_string();
        s.launch_args = Some(strs(&[
            "claude",
            "--mcp-config",
            "/home/lane-commysdk/synapse-home/lane.mcp.json",
            "--dangerously-load-development-channels",
            "server:synapse",
        ]));
        s.wsl = Some(WslTarget {
            distro: "lane-spike".to_string(),
            user: "lane-commysdk".to_string(),
            wrapper: "/usr/local/bin/lane-sandbox".to_string(),
            state_dir: None,
        });
        s
    }

    #[test]
    fn a_wsl_launch_is_hosted_on_windows_with_wsl_exe_as_the_hosts_child() {
        let p = prepare_launch(&wsl_spec("commy-sdk-rust"), "sonnet", None).unwrap();
        let hosted = p.argv.hosted();
        assert_eq!(hosted[1], "host");
        let sep = hosted
            .iter()
            .position(|a| a == "--")
            .expect("a -- before the child");
        let wsl_cmd = p
            .argv
            .wsl_command()
            .expect("a wsl launch has the wsl.exe command");
        assert_eq!(
            &hosted[sep + 1..],
            wsl_cmd,
            "the host runs exactly the wsl.exe command"
        );
        assert_eq!(wsl_cmd[0], "wsl.exe");
        assert!(
            wsl_cmd
                .windows(2)
                .any(|w| w[0] == "--cd" && w[1] == "/home/lane-commysdk/work/commy-sdk-rust"),
            "{wsl_cmd:?}"
        );
        // what runs inside the distro is the same claude argv a Windows lane gets: prompt first
        assert_eq!(p.argv[0], "claude");
        assert_eq!(p.argv[1], startup_prompt("commy-sdk-rust"));
        assert_eq!(wsl_cmd[wsl_cmd.len() - p.argv.len()..], p.argv[..]);
        assert!(p.carries_dev_channels_flag, "the dialog flag is still seen");
        // the terminal starts in a Windows folder, never the POSIX cwd
        assert_eq!(p.argv.cwd(), crate::wsl::host_start_dir());
        assert_ne!(p.argv.cwd(), "/home/lane-commysdk/work/commy-sdk-rust");
    }

    #[test]
    fn a_windows_launch_has_no_wsl_command() {
        let p = prepare_launch(&spec("overmind"), "sonnet", None).unwrap();
        assert!(p.argv.wsl_command().is_none());
        assert_eq!(p.argv.cwd(), "C:/x");
    }

    #[test]
    fn a_wsl_launch_is_refused_for_a_bad_target_a_windows_cwd_a_semicolon_or_opus() {
        let mut s = wsl_spec("commy-sdk-rust");
        s.wsl.as_mut().unwrap().user = "root".to_string();
        assert!(matches!(
            prepare_launch(&s, "sonnet", None),
            Err(RelaunchError::Wsl(_))
        ));
        let mut s = wsl_spec("commy-sdk-rust");
        s.cwd = "C:/Projects/commy-sdk-rust".to_string();
        assert!(matches!(
            prepare_launch(&s, "sonnet", None),
            Err(RelaunchError::Wsl(_))
        ));
        let mut s = wsl_spec("commy-sdk-rust");
        s.permission_mode = Some("a;b".into());
        assert!(matches!(
            prepare_launch(&s, "sonnet", None),
            Err(RelaunchError::Wsl(_)) | Err(RelaunchError::UnsafeArgument(_))
        ));
        assert!(matches!(
            prepare_launch(&wsl_spec("commy-sdk-rust"), "claude-opus-5", None),
            Err(RelaunchError::ModelRefused(_))
        ));
    }
}
