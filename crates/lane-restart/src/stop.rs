// SPDX-License-Identifier: MIT OR Apache-2.0

//! Stopping ONE lane: `lane-stop` (ARCHITECTURE-LAYERS-SPEC.md section 8b).
//! `lane-restart` is this, then `launch::launch_and_wait` (`lane-start`),
//! plus what only a restart needs (the authorisation, `assert-idle`, the
//! restart log). Each half is useful alone: agentlife stops a lane without
//! restarting it, and starts a dead one without stopping anything.

use crate::facts::{ProcessIdentity, SystemFacts};
use crate::relaunch::RelaunchError;
use std::time::Duration;

/// Time for the OS to finish tearing the process down before a new `claude`
/// claims the same working directory's lock.
pub const SETTLE: Duration = Duration::from_millis(500);

/// A small safety margin, not a guess: `facts.now()` is wall-clock time but a
/// process start time comes from the OS (on Linux, ticks since boot turned
/// into a Unix time); the two clocks can differ by a second or two. A CI
/// runner's real-process liveness test failed for exactly this before it
/// was added.
pub const CLOCK_MARGIN_SECS: u64 = 5;

/// What `stop_lane` hands the next step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stopped {
    /// Any process of this lane that started after this is a NEW one.
    pub launched_after_secs: u64,
}

/// Stops the lane's `claude` process, verified to still be the one that was
/// checked (a recycled pid is refused, never killed), and waits for the OS to
/// let go. `facts.kill_verified` is the verification that the process is the
/// right one and is gone; nothing else is asked of the OS here.
pub fn stop_lane(
    facts: &dyn SystemFacts,
    pid: u32,
    identity: &ProcessIdentity,
    sleep: &mut dyn FnMut(Duration),
) -> Result<Stopped, RelaunchError> {
    facts
        .kill_verified(pid, identity)
        .map_err(RelaunchError::Kill)?;
    sleep(SETTLE);
    Ok(Stopped {
        launched_after_secs: launched_after(facts.now()),
    })
}

/// The earliest start time (Unix seconds) a NEW process of the lane can have:
/// `now` minus the clock margin, never below zero. (The unsigned subtraction
/// matters: the same expression in signed arithmetic wraps to a huge number
/// for a clock within the margin of the epoch.)
pub fn launched_after(now: chrono::DateTime<chrono::Utc>) -> u64 {
    (now.timestamp().max(0) as u64).saturating_sub(CLOCK_MARGIN_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::KillError;
    use crate::relaunch::kill_and_relaunch_with;
    use crate::relaunch::RelaunchOutcome;
    use crate::testing::{log, state_at, RecordingFacts, ScriptedReader};
    use chrono::TimeZone;

    fn identity() -> ProcessIdentity {
        ProcessIdentity {
            start_time_secs: 99,
            exe: None,
        }
    }

    #[test]
    fn a_stop_kills_the_verified_pid_then_settles_then_reads_the_clock() {
        let l = log();
        let facts = RecordingFacts::new(&l);
        let mut slept = Vec::new();
        let stopped = stop_lane(&facts, 4242, &identity(), &mut |d| slept.push(d)).unwrap();
        assert_eq!(*l.borrow(), vec!["kill 4242 start=99", "now"]);
        assert_eq!(slept, vec![SETTLE]);
        assert_eq!(
            stopped.launched_after_secs,
            facts.now.timestamp() as u64 - CLOCK_MARGIN_SECS
        );
    }

    #[test]
    fn a_refused_kill_stops_there_with_no_wait_and_no_clock_read() {
        let l = log();
        let mut facts = RecordingFacts::new(&l);
        facts.kill_refuses = true;
        let mut slept = 0;
        let out = stop_lane(&facts, 4242, &identity(), &mut |_| slept += 1);
        assert!(matches!(
            out,
            Err(RelaunchError::Kill(KillError::IdentityChanged))
        ));
        assert_eq!(slept, 0);
        assert_eq!(*l.borrow(), vec!["kill 4242 start=99"]);
    }

    #[test]
    fn the_clock_margin_never_goes_below_zero() {
        let l = log();
        let mut facts = RecordingFacts::new(&l);
        facts.now = chrono::Utc.timestamp_opt(3, 0).unwrap();
        let s = stop_lane(&facts, 1, &identity(), &mut |_| {}).unwrap();
        assert_eq!(s.launched_after_secs, 0);
    }

    // -- a restart is a stop, then a start ---------------------------------

    fn restart(
        l: &crate::testing::Log,
        facts: &RecordingFacts,
        state: &crate::state::LaneState,
    ) -> Result<RelaunchOutcome, RelaunchError> {
        let reader = ScriptedReader::new(
            l,
            vec![Some(state_at(&state.role, "new", "UserPromptSubmit"))],
        );
        let log = std::rc::Rc::clone(l);
        kill_and_relaunch_with(
            facts,
            &reader,
            state,
            &identity(),
            "sonnet",
            None,
            &move |spec, argv| {
                log.borrow_mut()
                    .push(format!("spawn {} {}", spec.role, argv[0]));
                Ok(())
            },
            &mut |_| {},
            &mut || {},
        )
    }

    #[test]
    fn a_restart_stops_then_starts_in_that_order() {
        let l = log();
        let facts = RecordingFacts::new(&l);
        let st = state_at("overmind", "old", "Stop");
        assert_eq!(
            restart(&l, &facts, &st).unwrap(),
            RelaunchOutcome::Relaunched
        );
        assert_eq!(
            *l.borrow(),
            vec![
                "kill 4242 start=99",
                "now",
                "spawn overmind claude",
                "read overmind"
            ]
        );
    }

    #[test]
    fn a_restart_refuses_a_bad_launch_before_it_kills_anything() {
        let l = log();
        let facts = RecordingFacts::new(&l);
        let st = state_at("a&calc", "old", "Stop");
        assert!(matches!(
            restart(&l, &facts, &st),
            Err(RelaunchError::InvalidIdentifier(_))
        ));
        assert!(l.borrow().is_empty(), "touched something: {:?}", l.borrow());
    }

    #[test]
    fn a_restart_whose_kill_is_refused_never_starts_anything() {
        let l = log();
        let mut facts = RecordingFacts::new(&l);
        facts.kill_refuses = true;
        let st = state_at("overmind", "old", "Stop");
        assert!(matches!(
            restart(&l, &facts, &st),
            Err(RelaunchError::Kill(_))
        ));
        assert_eq!(*l.borrow(), vec!["kill 4242 start=99"]);
    }

    // -- a stop needs a spec on disk first (RESTART-TOOL-DESIGN.md 14.3) --------

    use crate::relaunch::kill_and_relaunch_persisting;
    use crate::spec::{self, DirSpecStore, SpecError, SpecFile, SpecState, SpecStore};

    /// Logs each write into the shared ordered log.
    struct LoggingStore {
        log: crate::testing::Log,
        fail: bool,
    }

    impl SpecStore for LoggingStore {
        fn write(&self, s: &SpecFile) -> Result<(), SpecError> {
            self.log.borrow_mut().push(format!("spec {:?}", s.state));
            if self.fail {
                Err(SpecError::Write("disk full".to_string()))
            } else {
                Ok(())
            }
        }
    }

    fn persisting_restart(
        facts: &RecordingFacts,
        l: &crate::testing::Log,
        state: &crate::state::LaneState,
        specs: &dyn SpecStore,
        policy_default: &str,
    ) -> Result<RelaunchOutcome, RelaunchError> {
        let reader = ScriptedReader::new(
            l,
            vec![Some(state_at(&state.role, "new", "UserPromptSubmit"))],
        );
        let log = std::rc::Rc::clone(l);
        kill_and_relaunch_persisting(
            facts,
            &reader,
            state,
            &identity(),
            policy_default,
            None,
            specs,
            &move |spec, argv| {
                log.borrow_mut()
                    .push(format!("spawn {} {}", spec.role, argv[0]));
                Ok(())
            },
            &mut |_| {},
            &mut || {},
        )
    }

    #[test]
    fn a_restart_writes_the_spec_stopping_before_the_kill_and_running_after_the_start() {
        let l = log();
        let facts = RecordingFacts::new(&l);
        let store = LoggingStore {
            log: std::rc::Rc::clone(&l),
            fail: false,
        };
        let st = state_at("overmind", "old", "Stop");
        assert_eq!(
            persisting_restart(&facts, &l, &st, &store, "sonnet").unwrap(),
            RelaunchOutcome::Relaunched
        );
        assert_eq!(
            *l.borrow(),
            vec![
                "spec Stopping",
                "kill 4242 start=99",
                "now",
                "spawn overmind claude",
                "read overmind",
                "spec Running"
            ]
        );
    }

    /// THE INVARIANT: at the moment of the kill the spec is on disk, parses, is `stopping`, and
    /// passes `prepare_launch`. Mutation target: remove the `specs.write(&stopping)` call (or move
    /// it after `stop_lane`) and this fails with "spec must exist at kill".
    #[test]
    fn at_the_moment_of_the_kill_a_complete_spec_is_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let l = log();
        let mut facts = RecordingFacts::new(&l);
        let seen = std::rc::Rc::new(std::cell::Cell::new(false));
        let (d, s) = (dir.path().to_path_buf(), std::rc::Rc::clone(&seen));
        facts.on_kill = Some(Box::new(move |_| {
            let on_disk = spec::read(&d, "overmind").expect("spec must exist at kill");
            assert_eq!(on_disk.state, SpecState::Stopping);
            on_disk.check().expect("the spec on disk must be complete");
            s.set(true);
        }));
        let store = DirSpecStore {
            dir: dir.path().to_path_buf(),
        };
        let st = state_at("overmind", "old", "Stop");
        persisting_restart(&facts, &l, &st, &store, "sonnet").unwrap();
        assert!(
            seen.get(),
            "the kill hook never ran, so nothing was checked"
        );
        assert_eq!(
            spec::read(dir.path(), "overmind").unwrap().state,
            SpecState::Running,
            "after a confirmed start the spec reads running"
        );
    }

    #[test]
    fn a_spec_that_cannot_be_written_means_nothing_is_killed() {
        let l = log();
        let facts = RecordingFacts::new(&l);
        let store = LoggingStore {
            log: std::rc::Rc::clone(&l),
            fail: true,
        };
        let st = state_at("overmind", "old", "Stop");
        let out = persisting_restart(&facts, &l, &st, &store, "sonnet");
        assert!(
            matches!(out, Err(RelaunchError::SpecNotWritten(_))),
            "{out:?}"
        );
        assert_eq!(*l.borrow(), vec!["spec Stopping"], "no kill, no spawn");
    }

    /// RESTART-TOOL-DESIGN.md 14.7 / 14.12 item 8: a restart launches from THE SPEC IT JUST
    /// WROTE, not from the state file. The state carries a flag the spec's allowlist drops; the
    /// launch must not. Mutation target: spawn `LaunchSpec::from(state)` again and this fails.
    #[test]
    fn a_restart_launches_from_the_spec_it_just_wrote_not_from_the_state_file() {
        let dir = tempfile::tempdir().unwrap();
        let l = log();
        let facts = RecordingFacts::new(&l);
        let store = DirSpecStore {
            dir: dir.path().to_path_buf(),
        };
        let mut st = state_at("overmind", "old", "Stop");
        st.launch_args = Some(vec![
            "claude".into(),
            "--model".into(),
            "sonnet".into(),
            "--not-on-the-allowlist".into(),
            "carries-something".into(),
        ]);
        let reader = ScriptedReader::new(&l, vec![Some(state_at("overmind", "new", "PreToolUse"))]);
        let spawned: std::rc::Rc<std::cell::RefCell<Option<crate::launch::LaunchSpec>>> =
            Default::default();
        let sp = std::rc::Rc::clone(&spawned);
        let out = kill_and_relaunch_persisting(
            &facts,
            &reader,
            &st,
            &identity(),
            "sonnet",
            None,
            &store,
            &move |spec, _| {
                *sp.borrow_mut() = Some(spec.clone());
                Ok(())
            },
            &mut |_| {},
            &mut || {},
        );
        assert_eq!(out.unwrap(), RelaunchOutcome::Relaunched);
        let spawned = spawned.borrow().clone().expect("nothing was spawned");
        let args = spawned.launch_args.clone().unwrap();
        assert!(
            !args.iter().any(|a| a.contains("not-on-the-allowlist")),
            "the launch carried a flag the spec dropped: {args:?}"
        );
        // And it is exactly what the spec on disk would launch (positive control: the spec
        // file is the stopping spec's successor, same launch parameters).
        let on_disk = spec::read(dir.path(), "overmind").unwrap();
        assert_eq!(spawned, on_disk.launch_spec());
    }

    #[test]
    fn a_launch_that_would_be_refused_writes_no_spec_and_kills_nothing() {
        let l = log();
        let facts = RecordingFacts::new(&l);
        let store = LoggingStore {
            log: std::rc::Rc::clone(&l),
            fail: false,
        };
        let st = state_at("a&calc", "old", "Stop");
        assert!(matches!(
            persisting_restart(&facts, &l, &st, &store, "sonnet"),
            Err(RelaunchError::InvalidIdentifier(_))
        ));
        assert!(l.borrow().is_empty(), "touched something: {:?}", l.borrow());
    }

    #[test]
    fn a_lane_whose_start_never_comes_up_is_left_with_a_stopping_spec_that_is_startable() {
        let dir = tempfile::tempdir().unwrap();
        let l = log();
        let mut facts = RecordingFacts::new(&l);
        facts.alive = false;
        let store = DirSpecStore {
            dir: dir.path().to_path_buf(),
        };
        let st = state_at("overmind", "old", "Stop");
        let reader = ScriptedReader::new(&l, vec![Some(state_at("overmind", "old", "Stop"))]);
        let out = kill_and_relaunch_persisting(
            &facts,
            &reader,
            &st,
            &identity(),
            "sonnet",
            None,
            &store,
            &|_, _| Ok(()),
            &mut |_| {},
            &mut || {},
        );
        assert!(out.is_err());
        let s = spec::role_status(dir.path(), "overmind", &RecordingFacts::new(&log()));
        assert_eq!(
            (s.state, s.startable),
            (spec::Lifecycle::Crashed, true),
            "{s:?}"
        );
    }

    /// The wait of a restart gets what the restart knows: the flag of the argv
    /// about to launch, and the default 15 minute total (here with a fake
    /// sleep, so no real wait).
    #[test]
    fn a_restart_of_a_lane_with_the_dev_channels_flag_reports_the_dialog() {
        let l = log();
        let facts = RecordingFacts::new(&l);
        let mut st = state_at("overmind", "old", "Stop");
        st.launch_args = Some(vec![
            "claude".into(),
            "--dangerously-load-development-channels".into(),
            "server:claude-peers".into(),
        ]);
        let reader =
            ScriptedReader::new(&l, vec![Some(state_at("overmind", "new", "SessionStart"))]);
        let mut slept = 0u32;
        let mut awaiting = 0;
        let out = kill_and_relaunch_with(
            &facts,
            &reader,
            &st,
            &identity(),
            "sonnet",
            None,
            &|_, _| Ok(()),
            &mut |_| slept += 1,
            &mut || awaiting += 1,
        );
        assert_eq!(out.unwrap(), RelaunchOutcome::AwaitingConfirmation);
        assert_eq!(awaiting, 1, "reported once");
        // 1 settle sleep, then 15 minutes in 1 s polls
        assert_eq!(slept, 1 + 15 * 60);
    }

    /// The session being replaced is the state's own: its later events are not
    /// the fresh session, so a restart does not call it done.
    #[test]
    fn a_restart_does_not_mistake_the_old_session_for_the_new_one() {
        let l = log();
        let mut facts = RecordingFacts::new(&l);
        facts.alive = false;
        let st = state_at("overmind", "old", "Stop");
        let reader = ScriptedReader::new(
            &l,
            vec![Some(state_at("overmind", "old", "UserPromptSubmit"))],
        );
        let out = kill_and_relaunch_with(
            &facts,
            &reader,
            &st,
            &identity(),
            "sonnet",
            None,
            &|_, _| Ok(()),
            &mut |_| {},
            &mut || {},
        );
        assert!(matches!(
            out,
            Err(RelaunchError::SessionNeverProcessedPrompt)
        ));
    }
}
