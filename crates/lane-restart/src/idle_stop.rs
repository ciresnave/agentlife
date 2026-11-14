// SPDX-License-Identifier: MIT OR Apache-2.0

//! The idle-stop verdict (SUPERVISION-DESIGN.md section 8a): would this lane be soft-stopped now?
//!
//! Pure and dry-run only. It reads nothing and stops nothing; a caller gathers the [`Inputs`] and
//! reports the [`Verdict`]. The numbers are [`Params`], which have NO default here: CireSnave has
//! not ruled on N, N2, W, K and M (board 437), so a caller must name them and nothing switches on.
//!
//! The self-idle assertion is the existing `lane-restart assert-idle` (`no_background_shells =
//! Some(true)`); any later busy hook clears it, so [`LaneState::is_idle`] already means "the lane
//! asserted idle and has done nothing since". Silence never makes a lane idle, it only DELAYS a stop.

use chrono::{DateTime, Duration, Utc};
use lane_state::state::LaneState;

/// The five numbers CireSnave has yet to rule on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    /// N: minimum quiet time before a soft stop.
    pub quiet: Duration,
    /// N2: the longer quiet time once the flap guard applies.
    pub quiet_flapping: Duration,
    /// W: how far back wakes are counted.
    pub window: Duration,
    /// K: wakes inside W at which the flap guard applies.
    pub wakes: u32,
    /// M: a WAIT due within this long blocks a stop.
    pub wait_ahead: Duration,
}

/// What the caller read about one lane.
#[derive(Debug, Clone)]
pub struct Inputs<'a> {
    pub state: &'a LaneState,
    pub now: DateTime<Utc>,
    /// When mail last reached the lane; `None` = none known.
    pub last_mail: Option<DateTime<Utc>>,
    /// Times the lane was woken by mail, inside the last W.
    pub wakes_in_window: u32,
    pub open_pr: bool,
    pub ready_roadmap_row: bool,
    /// The soonest WAIT wake time in its HANDOFF.
    pub next_wait_due: Option<DateTime<Utc>>,
    pub handoff_written: bool,
    /// The PM, Unpopped, Fuel and the tier-0 lanes, plus anything added by name.
    pub excluded: bool,
}

/// Why a lane stays up. Each names one term of the rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Keep {
    Excluded,
    /// The lane has not asserted idle (or did something since); carries `LaneState::idle_blockers`.
    NotIdle(Vec<String>),
    OpenPr,
    ReadyRoadmapRow,
    WaitDueSoon,
    HandoffMissing,
    /// Quiet for less than the required time: `(quiet_for, required)`.
    TooRecent(Duration, Duration),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    WouldStop,
    Keep(Vec<Keep>),
}

/// Every reason the lane stays up, or `WouldStop` when there is none.
pub fn verdict(i: &Inputs<'_>, p: &Params) -> Verdict {
    let mut why = Vec::new();
    if i.excluded {
        why.push(Keep::Excluded);
    }
    let blockers = i.state.idle_blockers();
    if !blockers.is_empty() {
        why.push(Keep::NotIdle(blockers));
    }
    if i.open_pr {
        why.push(Keep::OpenPr);
    }
    if i.ready_roadmap_row {
        why.push(Keep::ReadyRoadmapRow);
    }
    if i.next_wait_due.is_some_and(|d| d - i.now <= p.wait_ahead) {
        why.push(Keep::WaitDueSoon);
    }
    if !i.handoff_written {
        why.push(Keep::HandoffMissing);
    }
    let since = i
        .last_mail
        .map_or(i.state.updated_at, |m| m.max(i.state.updated_at));
    let quiet_for = i.now - since;
    let required = if i.wakes_in_window >= p.wakes {
        p.quiet_flapping
    } else {
        p.quiet
    };
    if quiet_for < required {
        why.push(Keep::TooRecent(quiet_for, required));
    }
    if why.is_empty() {
        Verdict::WouldStop
    } else {
        Verdict::Keep(why)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(min: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-14T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + Duration::minutes(min)
    }

    fn state(updated_min: i64) -> LaneState {
        LaneState {
            role: "capant".into(),
            session_id: "s".into(),
            pid: 1,
            pid_start_secs: None,
            cwd: "C:/x".into(),
            name: None,
            model: None,
            permission_mode: None,
            remote_control: false,
            busy: false,
            subagents_running: 0,
            no_background_shells: Some(true),
            launch_args: None,
            updated_at: at(updated_min),
            updated_by_event: "AssertIdle".into(),
        }
    }

    fn params() -> Params {
        Params {
            quiet: Duration::minutes(30),
            quiet_flapping: Duration::hours(2),
            window: Duration::hours(3),
            wakes: 3,
            wait_ahead: Duration::minutes(60),
        }
    }

    fn base(s: &LaneState) -> Inputs<'_> {
        Inputs {
            state: s,
            now: at(40),
            last_mail: None,
            wakes_in_window: 0,
            open_pr: false,
            ready_roadmap_row: false,
            next_wait_due: None,
            handoff_written: true,
            excluded: false,
        }
    }

    #[test]
    fn an_asserted_idle_lane_quiet_for_n_minutes_would_stop() {
        let s = state(0);
        assert_eq!(verdict(&base(&s), &params()), Verdict::WouldStop);
    }

    #[test]
    fn a_lane_that_never_asserted_idle_is_never_stopped_however_quiet() {
        let mut s = state(0);
        s.no_background_shells = None;
        let mut i = base(&s);
        i.now = at(10_000);
        assert_eq!(
            verdict(&i, &params()),
            Verdict::Keep(vec![Keep::NotIdle(vec!["shells_never_asserted".into()])])
        );
    }

    #[test]
    fn mail_resets_the_quiet_clock() {
        let s = state(0);
        let mut i = base(&s);
        i.last_mail = Some(at(25));
        assert_eq!(
            verdict(&i, &params()),
            Verdict::Keep(vec![Keep::TooRecent(
                Duration::minutes(15),
                Duration::minutes(30)
            )])
        );
    }

    #[test]
    fn a_lane_woken_k_times_needs_the_longer_quiet_time() {
        let s = state(0);
        let mut i = base(&s);
        i.wakes_in_window = 3;
        assert_eq!(
            verdict(&i, &params()),
            Verdict::Keep(vec![Keep::TooRecent(
                Duration::minutes(40),
                Duration::hours(2)
            )])
        );
        i.wakes_in_window = 2;
        assert_eq!(verdict(&i, &params()), Verdict::WouldStop);
        i.wakes_in_window = 3;
        i.now = at(120);
        assert_eq!(verdict(&i, &params()), Verdict::WouldStop);
    }

    #[test]
    fn a_wait_due_inside_m_blocks_and_one_beyond_it_does_not() {
        let s = state(0);
        let mut i = base(&s);
        i.next_wait_due = Some(at(40 + 60));
        assert_eq!(
            verdict(&i, &params()),
            Verdict::Keep(vec![Keep::WaitDueSoon])
        );
        i.next_wait_due = Some(at(40 + 61));
        assert_eq!(verdict(&i, &params()), Verdict::WouldStop);
    }

    #[test]
    fn every_reason_is_reported_not_just_the_first() {
        let mut s = state(35);
        s.busy = true;
        let mut i = base(&s);
        i.excluded = true;
        i.open_pr = true;
        i.ready_roadmap_row = true;
        i.handoff_written = false;
        i.next_wait_due = Some(at(41));
        let Verdict::Keep(why) = verdict(&i, &params()) else {
            panic!("must keep");
        };
        assert_eq!(why.len(), 7, "{why:?}");
    }
}
