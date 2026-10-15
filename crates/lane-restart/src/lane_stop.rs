// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart stop` - RESTART-TOOL-DESIGN.md 14.3: suspend a running lane.
//!
//! **The invariant (the PM's): a lane may be suspended only if it can be started again.** So the
//! order is fixed, and it is the whole point of this module:
//!
//! 1. every check, none of which touches the process;
//! 2. the complete launch spec written durably and read back, `state: "stopping"`;
//! 3. the verified kill (`stop::stop_lane`);
//! 4. the spec rewritten as `suspended`.
//!
//! The decision (`plan_stop`) is separate from the effect (`execute_stop`), as in `start.rs`,
//! so each refusal is testable without a process.
//!
//! Who may stop: the PM session (verified by process ancestry, 14.8) for another lane, or the
//! lane itself with `--self`. Both need a HANDOFF written during this session; there is
//! deliberately no flag that skips that.

use crate::authorize::{self, Refusal, Request, Target};
use crate::facts::{ProcessIdentity, SystemFacts};
use crate::relaunch::{resolve_launch_model, valid_identifier, RelaunchError};
use crate::spec::{self, find_handoff, SpecFile, SpecState, SpecStore};
use crate::start::{validate_reason, CallerError, PM_ROLE};
use crate::state::LaneState;
use crate::stop::stop_lane;
use chrono::{DateTime, Utc};
use std::path::Path;
use std::time::Duration;

pub struct StopRequest<'a> {
    pub role: &'a str,
    pub reason: &'a str,
    /// `--self`: the lane stopping itself. Otherwise the caller must be the PM session.
    pub self_stop: bool,
    /// The `claude` process the caller runs under, from its own ancestry; only `--self` uses it.
    pub caller_claude_pid: Option<u32>,
    /// `--yes`: without it a stop of another lane is a dry run (the same rule as a restart).
    pub confirmed: bool,
    pub dry_run: bool,
}

#[derive(Debug)]
pub enum StopRefusal {
    /// A stop of another lane by a caller that is not the PM session.
    NotPm(Result<String, CallerError>),
    InvalidRole(String),
    /// The PM could not be started again by `lane-start` (it is started by a person), so by the
    /// invariant it may not be suspended. A PM restarts through `lane-restart --self`.
    PmRole,
    Reason(&'static str),
    /// The target failed the identity, idle or `--self` check.
    Target(Refusal),
    /// No HANDOFF file in the lane's directory: there is deliberately no override.
    NoHandoff {
        cwd: String,
    },
    /// The HANDOFF is older than the session being stopped, so it is not this session's.
    HandoffBeforeSession {
        path: String,
        modified: DateTime<Utc>,
        session_start_secs: u64,
    },
    /// The HANDOFF was written after the lane asserted idle: the lane did more work after
    /// claiming it had none.
    HandoffAfterIdle {
        path: String,
        modified: DateTime<Utc>,
        claimed: DateTime<Utc>,
    },
    /// A spec that `prepare_launch` accepts cannot be built from the live lane.
    Spec(String),
}

impl std::fmt::Display for StopRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StopRefusal::NotPm(Ok(role)) => write!(
                f,
                "only the PM session may stop another lane (or a lane itself with --self); this \
                 command is running under {role:?}"
            ),
            StopRefusal::NotPm(Err(e)) => write!(
                f,
                "only the PM session may stop another lane, and the caller could not be \
                 verified: {e}"
            ),
            StopRefusal::InvalidRole(r) => write!(f, "not a plain role identifier: {r:?}"),
            StopRefusal::PmRole => write!(
                f,
                "the PM cannot be suspended: `lane-restart start` refuses role pm (it is started \
                 by a person), so a suspended PM would have no way back. Use `lane-restart \
                 --role pm --self` to restart it"
            ),
            StopRefusal::Reason(why) => write!(f, "{why}"),
            StopRefusal::Target(r) => write!(f, "{r}"),
            StopRefusal::NoHandoff { cwd } => write!(
                f,
                "no HANDOFF.md (or <ROLE>-HANDOFF.md) in {cwd}: a lane is stopped only after it \
                 has written its HANDOFF, and there is no override"
            ),
            StopRefusal::HandoffBeforeSession {
                path,
                modified,
                session_start_secs,
            } => write!(
                f,
                "{path} was last written {modified}, before this session started \
                 (start time {session_start_secs}): it is not this session's HANDOFF"
            ),
            StopRefusal::HandoffAfterIdle {
                path,
                modified,
                claimed,
            } => write!(
                f,
                "{path} was written {modified}, after the lane asserted idle at {claimed}: it \
                 kept working after claiming it had stopped"
            ),
            StopRefusal::Spec(why) => write!(f, "{why}"),
        }
    }
}

/// A stop that passed every check: nothing has been written or killed yet.
#[derive(Debug)]
pub struct StopPlan {
    pub state: LaneState,
    /// Recorded at decision time and re-checked immediately before the kill.
    pub identity: ProcessIdentity,
    /// The spec to write before the kill (`state: stopping`), complete by construction.
    pub stopping: SpecFile,
    /// The model this spec replaced, when the lane was on Opus, an unknown model or none.
    pub pinned_from: Option<String>,
    pub handoff_path: String,
    /// False for a dry run, and for another lane's stop without `--yes`.
    pub will_act: bool,
}

/// 14.3 step 1 / 14.8: acting on ANOTHER lane (`self_target == false`) is the PM session's alone,
/// established by process ancestry, never by `LANE_ROLE` or a flag. `--self` is checked by
/// `authorize::decide` instead (the caller's claude must be the recorded process). Shared by
/// `stop` and by the restart of another lane, so the two cannot disagree about who the PM is.
pub fn require_pm_for_other(
    self_target: bool,
    caller: Result<String, CallerError>,
) -> Result<(), StopRefusal> {
    if self_target {
        return Ok(());
    }
    match caller {
        Ok(role) if role == PM_ROLE => Ok(()),
        other => Err(StopRefusal::NotPm(other)),
    }
}

/// The HANDOFF precondition (14.3 step 2). `idle_claimed_at` is `None` for `--self`: a lane that
/// stops itself writes its HANDOFF and stops in one go, so there is no earlier claim to compare.
pub fn check_handoff(
    cwd: &str,
    role: &str,
    session_start_secs: u64,
    idle_claimed_at: Option<DateTime<Utc>>,
) -> Result<String, StopRefusal> {
    let (path, modified) = find_handoff(cwd, role).ok_or_else(|| StopRefusal::NoHandoff {
        cwd: cwd.to_string(),
    })?;
    if modified.timestamp().max(0) as u64 <= session_start_secs.saturating_sub(1) {
        // `<=` on (start - 1): a HANDOFF written in the very second the process started is
        // not distinguishable from one written just before it at whole-second precision.
        return Err(StopRefusal::HandoffBeforeSession {
            path,
            modified,
            session_start_secs,
        });
    }
    if let Some(claimed) = idle_claimed_at {
        if modified > claimed {
            return Err(StopRefusal::HandoffAfterIdle {
                path,
                modified,
                claimed,
            });
        }
    }
    Ok(path)
}

/// Every check, in the order of 14.3; nothing is written or killed.
pub fn plan_stop(
    dir: &Path,
    req: &StopRequest,
    caller: Result<String, CallerError>,
    facts: &dyn SystemFacts,
    policy_default: &str,
) -> Result<StopPlan, StopRefusal> {
    if !valid_identifier(req.role) {
        return Err(StopRefusal::InvalidRole(req.role.to_string()));
    }
    if req.role == PM_ROLE {
        return Err(StopRefusal::PmRole);
    }
    validate_reason(req.reason).map_err(StopRefusal::Reason)?;
    require_pm_for_other(req.self_stop, caller)?;
    let target = if req.self_stop {
        Target::Myself {
            role: req.role.to_string(),
            caller_pid: req.caller_claude_pid,
        }
    } else {
        Target::Other {
            role: req.role.to_string(),
        }
    };
    let decided = authorize::decide(
        &Request {
            target,
            confirmed: req.confirmed,
            dry_run: req.dry_run,
        },
        facts,
        dir,
    )
    .map_err(StopRefusal::Target)?;
    build_stop_plan(
        decided.state,
        decided.identity,
        decided.will_act,
        req.reason,
        req.self_stop,
        facts.now(),
        policy_default,
    )
}

/// The checks that need a decided target: the HANDOFF and a complete spec. Separate from
/// `plan_stop` so they are testable without a process table.
pub fn build_stop_plan(
    state: LaneState,
    identity: ProcessIdentity,
    will_act: bool,
    reason: &str,
    self_stop: bool,
    now: DateTime<Utc>,
    policy_default: &str,
) -> Result<StopPlan, StopRefusal> {
    // Another lane's idle claim is `updated_at` here: the idle gate has already required
    // `no_background_shells`, which only the assert-idle event sets and every later turn clears.
    let idle_claimed_at = (!self_stop).then_some(state.updated_at);
    let handoff_path = check_handoff(
        &state.cwd,
        &state.role,
        identity.start_time_secs,
        idle_claimed_at,
    )?;
    let launch = crate::launch::LaunchSpec::from(&state);
    // `allow_opus = true` only to read the string: an Opus lane is suspended with its model
    // *replaced* (`pin_model`), because the spec can never carry Opus and an Opus lane must
    // still be stoppable. The replacement is reported, never silent.
    let resolved = resolve_launch_model(launch.launch_args.as_ref(), policy_default, true)
        .map_err(|e: RelaunchError| StopRefusal::Spec(e.to_string()))?;
    let (model, pinned_from) = spec::pin_model(&resolved);
    let mut stopping = spec::build(&spec::SpecInputs {
        launch: &launch,
        model: &model,
        session_id: Some(&state.session_id),
        pid_start_secs: Some(identity.start_time_secs),
        state: SpecState::Stopping,
        written_by: "stop",
        now,
    })
    .map_err(|e| StopRefusal::Spec(e.to_string()))?;
    stopping.stop_reason = Some(reason.trim().to_string());
    stopping
        .check()
        .map_err(|e| StopRefusal::Spec(e.to_string()))?;
    Ok(StopPlan {
        state,
        identity,
        stopping,
        pinned_from,
        handoff_path,
        will_act,
    })
}

#[derive(Debug)]
pub enum StopError {
    /// The spec could not be written and read back: NOTHING was killed.
    SpecNotWritten(String),
    /// The kill was refused or failed. The spec is `stopping`, and the process is still alive
    /// (or was never the verified one), so the lane reads as running.
    Kill(RelaunchError),
}

impl std::fmt::Display for StopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StopError::SpecNotWritten(why) => write!(
                f,
                "the launch spec could not be written, so nothing was stopped: {why}"
            ),
            StopError::Kill(e) => write!(f, "{e}"),
        }
    }
}

#[derive(Debug)]
pub struct StopDone {
    /// Set when the process is gone but the spec could not be marked `suspended` (exit 4): the
    /// `stopping` spec is still complete and reads as `crashed`, which is startable.
    pub spec_warning: Option<String>,
}

/// Writes the complete spec, kills the verified process, then marks the spec `suspended`.
pub fn execute_stop(
    plan: &StopPlan,
    facts: &dyn SystemFacts,
    specs: &dyn SpecStore,
    sleep: &mut dyn FnMut(Duration),
) -> Result<StopDone, StopError> {
    specs
        .write(&plan.stopping)
        .map_err(|e| StopError::SpecNotWritten(e.to_string()))?;
    stop_lane(facts, plan.state.pid, &plan.identity, sleep).map_err(StopError::Kill)?;
    let mut suspended = plan.stopping.clone();
    suspended.state = SpecState::Suspended;
    suspended.written_at = facts.now();
    let spec_warning = specs.write(&suspended).err().map(|e| e.to_string());
    Ok(StopDone { spec_warning })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::DirSpecStore;
    use crate::testing::{log, state_at, RecordingFacts};
    use chrono::TimeZone;
    use std::cell::RefCell;
    use std::rc::Rc;
    use tempfile::tempdir;

    fn identity() -> ProcessIdentity {
        ProcessIdentity {
            start_time_secs: 1_000,
            exe: None,
        }
    }

    /// A state whose cwd is a real temp directory, so a HANDOFF can really be there.
    fn state_in(cwd: &Path) -> LaneState {
        let mut s = state_at("baracuda", "sess-1", "AssertIdle");
        s.cwd = cwd.to_string_lossy().into_owned();
        s.launch_args = Some(vec!["claude".into(), "--model".into(), "sonnet".into()]);
        s
    }

    fn write_handoff(cwd: &Path, secs_after_start: i64) -> DateTime<Utc> {
        let p = cwd.join("HANDOFF.md");
        std::fs::write(&p, "state\n").unwrap();
        let when = Utc.timestamp_opt(1_000 + secs_after_start, 0).unwrap();
        let f = std::fs::File::options().write(true).open(&p).unwrap();
        f.set_modified(when.into()).unwrap();
        when
    }

    fn plan_for(state: LaneState, self_stop: bool) -> Result<StopPlan, StopRefusal> {
        let now = Utc.timestamp_opt(5_000, 0).unwrap();
        build_stop_plan(
            state,
            identity(),
            true,
            "end of shift",
            self_stop,
            now,
            "sonnet",
        )
    }

    // --- the HANDOFF precondition --------------------------------------------------------

    #[test]
    fn no_handoff_is_a_refusal_with_no_override() {
        let d = tempdir().unwrap();
        let r = plan_for(state_in(d.path()), false);
        assert!(matches!(r, Err(StopRefusal::NoHandoff { .. })), "{r:?}");
    }

    #[test]
    fn a_handoff_from_before_the_session_is_refused() {
        let d = tempdir().unwrap();
        write_handoff(d.path(), -600);
        let r = plan_for(state_in(d.path()), false);
        assert!(
            matches!(r, Err(StopRefusal::HandoffBeforeSession { .. })),
            "{r:?}"
        );
    }

    #[test]
    fn a_handoff_written_after_the_idle_claim_is_refused() {
        let d = tempdir().unwrap();
        let mut s = state_in(d.path());
        // Claimed idle at 1_100; the HANDOFF was touched at 1_500.
        s.updated_at = Utc.timestamp_opt(1_100, 0).unwrap();
        write_handoff(d.path(), 500);
        let r = plan_for(s, false);
        assert!(
            matches!(r, Err(StopRefusal::HandoffAfterIdle { .. })),
            "{r:?}"
        );
    }

    #[test]
    fn a_handoff_between_session_start_and_the_idle_claim_passes() {
        let d = tempdir().unwrap();
        let mut s = state_in(d.path());
        s.updated_at = Utc.timestamp_opt(1_900, 0).unwrap();
        write_handoff(d.path(), 500);
        let plan = plan_for(s, false).expect("a fresh HANDOFF before the claim is allowed");
        assert_eq!(plan.stopping.state, SpecState::Stopping);
        assert_eq!(plan.stopping.written_by, "stop");
        assert_eq!(plan.stopping.stop_reason.as_deref(), Some("end of shift"));
        assert_eq!(plan.stopping.stopped_session_id.as_deref(), Some("sess-1"));
        assert_eq!(plan.stopping.stopped_pid_start_secs, Some(1_000));
    }

    #[test]
    fn a_self_stop_has_no_idle_claim_to_compare_but_still_needs_a_fresh_handoff() {
        let d = tempdir().unwrap();
        let mut s = state_in(d.path());
        s.updated_at = Utc.timestamp_opt(1_100, 0).unwrap();
        write_handoff(d.path(), 500); // after `updated_at`: fine for --self
        assert!(plan_for(s.clone(), true).is_ok());
        write_handoff(d.path(), -600);
        assert!(matches!(
            plan_for(s, true),
            Err(StopRefusal::HandoffBeforeSession { .. })
        ));
    }

    // --- the spec ------------------------------------------------------------------------

    #[test]
    fn an_opus_lane_is_suspended_with_its_model_replaced_and_the_replacement_reported() {
        let d = tempdir().unwrap();
        write_handoff(d.path(), 100);
        let mut s = state_in(d.path());
        s.updated_at = Utc.timestamp_opt(1_900, 0).unwrap();
        s.launch_args = Some(vec!["claude".into(), "--model".into(), "opus".into()]);
        let plan = plan_for(s, false).unwrap();
        assert_eq!(plan.stopping.model, "sonnet");
        assert_eq!(plan.pinned_from.as_deref(), Some("opus"));
        assert!(plan.stopping.check().is_ok());
    }

    #[test]
    fn a_secret_shaped_inline_config_refuses_the_stop_rather_than_storing_it() {
        let d = tempdir().unwrap();
        write_handoff(d.path(), 100);
        let mut s = state_in(d.path());
        s.updated_at = Utc.timestamp_opt(1_900, 0).unwrap();
        s.launch_args = Some(vec![
            "claude".into(),
            "--model".into(),
            "sonnet".into(),
            "--mcp-config".into(),
            "{\"token\":\"hunter2\"}".into(),
        ]);
        let r = plan_for(s, false);
        assert!(matches!(r, Err(StopRefusal::Spec(_))), "{r:?}");
    }

    // --- the order: the invariant --------------------------------------------------------

    fn ready_plan(dir: &Path) -> StopPlan {
        write_handoff(dir, 100);
        let mut s = state_in(dir);
        s.updated_at = Utc.timestamp_opt(1_900, 0).unwrap();
        plan_for(s, false).unwrap()
    }

    /// The invariant, tested at the moment of the kill: the spec is on disk, parses, is
    /// `stopping`, and a start would accept it. A mutant that drops the write, or that kills
    /// before writing, fails this (see the PR's mutation record).
    #[test]
    fn lane_stop_never_kills_without_a_complete_spec_on_disk() {
        let d = tempdir().unwrap();
        let plan = ready_plan(d.path());
        let specs = DirSpecStore {
            dir: d.path().to_path_buf(),
        };
        let seen: Rc<RefCell<Option<Result<SpecFile, String>>>> = Rc::new(RefCell::new(None));
        let l = log();
        let mut facts = RecordingFacts::new(&l);
        let (dir, seen2) = (d.path().to_path_buf(), Rc::clone(&seen));
        facts.on_kill = Some(Box::new(move |_| {
            *seen2.borrow_mut() = Some(spec::read(&dir, "baracuda").map_err(|e| e.to_string()));
        }));
        let done = execute_stop(&plan, &facts, &specs, &mut |_| {}).unwrap();
        assert!(done.spec_warning.is_none());

        let at_kill = seen
            .borrow_mut()
            .take()
            .expect("the kill was never reached")
            .expect("no readable spec on disk at the moment of the kill");
        assert_eq!(at_kill.state, SpecState::Stopping);
        assert!(at_kill.check().is_ok(), "a start would refuse it");
        assert_eq!(at_kill.stop_reason.as_deref(), Some("end of shift"));

        let after = spec::read(d.path(), "baracuda").unwrap();
        assert_eq!(after.state, SpecState::Suspended);
        assert_eq!(after.stopped_session_id.as_deref(), Some("sess-1"));
        // The same launch, apart from the state and the clock.
        let mut as_suspended = at_kill.clone();
        as_suspended.state = SpecState::Suspended;
        as_suspended.written_at = after.written_at;
        assert_eq!(after, as_suspended);
    }

    /// A spec that cannot be written leaves the process alone: the kill is never reached.
    #[test]
    fn a_spec_that_cannot_be_written_means_nothing_is_killed() {
        let d = tempdir().unwrap();
        let plan = ready_plan(d.path());
        // The "directory" is a file, so the atomic write cannot create its temp file.
        let blocker = d.path().join("not-a-dir");
        std::fs::write(&blocker, "x").unwrap();
        let specs = DirSpecStore { dir: blocker };
        let l = log();
        let facts = RecordingFacts::new(&l);
        let r = execute_stop(&plan, &facts, &specs, &mut |_| {});
        assert!(matches!(r, Err(StopError::SpecNotWritten(_))), "{r:?}");
        assert!(
            l.borrow().iter().all(|e| !e.starts_with("kill")),
            "a kill was issued without a spec: {:?}",
            l.borrow()
        );
    }

    /// A refused kill (a recycled pid) leaves the lane `stopping`, never `suspended`: it is
    /// not suspended, and the spec on disk must not say it is.
    #[test]
    fn a_refused_kill_never_marks_the_lane_suspended() {
        let d = tempdir().unwrap();
        let plan = ready_plan(d.path());
        let specs = DirSpecStore {
            dir: d.path().to_path_buf(),
        };
        let l = log();
        let mut facts = RecordingFacts::new(&l);
        facts.kill_refuses = true;
        let r = execute_stop(&plan, &facts, &specs, &mut |_| {});
        assert!(matches!(r, Err(StopError::Kill(_))), "{r:?}");
        let on_disk = spec::read(d.path(), "baracuda").unwrap();
        assert_eq!(on_disk.state, SpecState::Stopping);
    }

    /// The final `suspended` write failing is reported (exit 4 in the CLI), with the process
    /// already gone and the earlier spec still complete.
    #[test]
    fn a_failed_suspended_write_is_a_warning_with_the_stopping_spec_intact() {
        struct FailSecond(DirSpecStore, std::cell::Cell<u32>);
        impl SpecStore for FailSecond {
            fn write(&self, s: &SpecFile) -> Result<(), spec::SpecError> {
                let n = self.1.get();
                self.1.set(n + 1);
                if n == 0 {
                    self.0.write(s)
                } else {
                    Err(spec::SpecError::Write("disk full".into()))
                }
            }
        }
        let d = tempdir().unwrap();
        let plan = ready_plan(d.path());
        let specs = FailSecond(
            DirSpecStore {
                dir: d.path().to_path_buf(),
            },
            std::cell::Cell::new(0),
        );
        let l = log();
        let facts = RecordingFacts::new(&l);
        let done = execute_stop(&plan, &facts, &specs, &mut |_| {}).unwrap();
        assert!(done.spec_warning.as_deref().unwrap().contains("disk full"));
        let on_disk = spec::read(d.path(), "baracuda").unwrap();
        assert_eq!(on_disk.state, SpecState::Stopping);
        assert!(on_disk.check().is_ok());
    }

    // --- who may stop --------------------------------------------------------------------

    fn request<'a>(role: &'a str, self_stop: bool) -> StopRequest<'a> {
        StopRequest {
            role,
            reason: "end of shift",
            self_stop,
            caller_claude_pid: None,
            confirmed: true,
            dry_run: false,
        }
    }

    #[test]
    fn a_stop_of_another_lane_by_a_non_pm_caller_is_refused_before_anything_else() {
        let d = tempdir().unwrap();
        let l = log();
        let facts = RecordingFacts::new(&l);
        for caller in [
            Ok("synapse".to_string()),
            Ok("overmind".to_string()),
            Err(CallerError::NoClaudeAncestor),
        ] {
            let r = plan_stop(
                d.path(),
                &request("baracuda", false),
                caller,
                &facts,
                "sonnet",
            );
            assert!(matches!(r, Err(StopRefusal::NotPm(_))), "{r:?}");
        }
        // The same inputs from the PM get past the caller check (and stop at the missing
        // state file, which the caller check precedes): the positive control.
        let r = plan_stop(
            d.path(),
            &request("baracuda", false),
            Ok("pm".to_string()),
            &facts,
            "sonnet",
        );
        assert!(
            matches!(r, Err(StopRefusal::Target(Refusal::NoStateFile(_)))),
            "{r:?}"
        );
        assert!(l.borrow().is_empty(), "no fact was needed to refuse");
    }

    /// The rule the restart of another lane shares with `stop` (design 14.8). Positive control:
    /// the PM passes, and `--self` is not this check's business.
    #[test]
    fn only_the_pm_session_may_act_on_another_lane() {
        assert!(require_pm_for_other(false, Ok("pm".into())).is_ok());
        assert!(require_pm_for_other(true, Err(CallerError::NoClaudeAncestor)).is_ok());
        for caller in [
            Ok("synapse".to_string()),
            Err(CallerError::NoClaudeAncestor),
            Err(CallerError::Unrecognised(7)),
        ] {
            assert!(
                matches!(
                    require_pm_for_other(false, caller),
                    Err(StopRefusal::NotPm(_))
                ),
                "a non-PM caller was let through"
            );
        }
    }

    #[test]
    fn the_pm_role_cannot_be_suspended_even_with_self() {
        let d = tempdir().unwrap();
        let facts = RecordingFacts::new(&log());
        for self_stop in [false, true] {
            let r = plan_stop(
                d.path(),
                &request("pm", self_stop),
                Ok("pm".to_string()),
                &facts,
                "sonnet",
            );
            assert!(matches!(r, Err(StopRefusal::PmRole)), "{r:?}");
        }
    }

    #[test]
    fn a_missing_reason_or_a_bad_role_is_refused() {
        let d = tempdir().unwrap();
        let facts = RecordingFacts::new(&log());
        let mut req = request("baracuda", false);
        req.reason = "   ";
        let r = plan_stop(d.path(), &req, Ok("pm".into()), &facts, "sonnet");
        assert!(matches!(r, Err(StopRefusal::Reason(_))), "{r:?}");
        let r = plan_stop(
            d.path(),
            &request("a;b", false),
            Ok("pm".into()),
            &facts,
            "sonnet",
        );
        assert!(matches!(r, Err(StopRefusal::InvalidRole(_))), "{r:?}");
    }
}
