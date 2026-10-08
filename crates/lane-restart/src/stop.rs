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
            false,
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
            false,
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
            false,
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
