// SPDX-License-Identifier: MIT OR Apache-2.0

//! Waits that cannot be forgotten: the `WAIT:` line grammar of a lane's HANDOFF.
//!
//! CireSnave, 2026-11-01: "Perhaps if a lane is waiting for something it must be required to set a
//! timer or arrange for a callback that it can't ignore?" A lane that waits on a merge or a CI run
//! and writes "start after X merges" in its HANDOFF has written a note nothing re-reads (Unpopped
//! sat about 10 hours idle on one). A wait is therefore a line with three parts, each of which a
//! program can read:
//!
//! ```text
//! WAIT: <event> | check: <command> | wake: <RFC3339 UTC time>
//! ```
//!
//! - `<event>` says what is awaited, in words.
//! - `check:` is a command that exits 0 when the event has happened (a supervisor can run it).
//! - `wake:` is an absolute time at which the lane must look again, whatever the check says.
//!   Absolute, never "in 20 minutes": a relative time in a file goes stale the moment the lane
//!   restarts, which is exactly when a wait is lost.
//!
//! The line starts the line (after whitespace and an optional `- ` or `* ` list marker); prose
//! that merely mentions `WAIT:` mid-sentence is not a wait. `assert-idle` refuses a HANDOFF with a
//! malformed wait ([`refusal`]) and `lane-restart waits` prints what each wait needs ([`report`]).
//! Nothing here arms a timer: `lane-restart` cannot see one inside a `claude` session, and the
//! restart that follows `assert-idle` ends that session. The lane re-arms from the HANDOFF at its
//! next start.

use chrono::{DateTime, Utc};

const MARK: &str = "WAIT:";

/// One well-formed wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wait {
    /// 1-based line number in the HANDOFF.
    pub line: usize,
    pub event: String,
    pub check: String,
    pub wake: DateTime<Utc>,
}

/// A wait line that cannot be used, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Malformed {
    pub line: usize,
    pub why: String,
}

/// Every `WAIT:` line in `text`, in order.
pub fn parse(text: &str) -> Vec<Result<Wait, Malformed>> {
    text.lines()
        .enumerate()
        .filter_map(|(i, raw)| {
            let rest = wait_body(raw)?;
            Some(parse_line(i + 1, rest))
        })
        .collect()
}

/// The text after `WAIT:` when `line` is a wait line.
fn wait_body(line: &str) -> Option<&str> {
    let t = line.trim_start();
    let t = t
        .strip_prefix("- ")
        .or_else(|| t.strip_prefix("* "))
        .unwrap_or(t)
        .trim_start();
    t.strip_prefix(MARK)
}

fn parse_line(line: usize, body: &str) -> Result<Wait, Malformed> {
    let bad = |why: &str| Malformed {
        line,
        why: why.to_string(),
    };
    let mut parts = body.split(" | ");
    let event = parts.next().unwrap_or_default().trim();
    if event.is_empty() {
        return Err(bad("no event: say what is awaited after `WAIT:`"));
    }
    let (mut check, mut wake) = (None, None);
    for p in parts {
        let p = p.trim();
        if let Some(c) = p.strip_prefix("check:") {
            check = Some(c.trim());
        } else if let Some(w) = p.strip_prefix("wake:") {
            wake = Some(w.trim());
        } else {
            return Err(bad(&format!(
                "unknown part `{p}`: expected `check: <command>` and `wake: <time>`"
            )));
        }
    }
    let check = check.filter(|c| !c.is_empty()).ok_or_else(|| {
        bad("no `check: <command>`: a command that exits 0 when the event has happened")
    })?;
    let wake = wake
        .filter(|w| !w.is_empty())
        .ok_or_else(|| bad("no `wake: <time>`: the lane must choose when it looks again"))?;
    let wake = DateTime::parse_from_rfc3339(wake)
        .map_err(|_| {
            bad(&format!(
                "`wake: {wake}` is not an absolute RFC3339 time such as 2026-11-01T18:00:00Z"
            ))
        })?
        .with_timezone(&Utc);
    Ok(Wait {
        line,
        event: event.to_string(),
        check: check.to_string(),
        wake,
    })
}

/// `assert-idle`'s verdict on a HANDOFF at `now`: `Err` names every malformed wait (a refusal);
/// `Ok` carries a warning for each wait whose time has already passed.
pub fn refusal(text: &str, now: DateTime<Utc>) -> Result<Vec<String>, String> {
    let parsed = parse(text);
    let bad: Vec<String> = parsed
        .iter()
        .filter_map(|r| r.as_ref().err())
        .map(|m| format!("  line {}: {}", m.line, m.why))
        .collect();
    if !bad.is_empty() {
        return Err(format!(
            "HANDOFF has {} wait line(s) that cannot be woken, so this lane would forget them \
             (format: `WAIT: <event> | check: <command> | wake: <RFC3339 UTC time>`):\n{}",
            bad.len(),
            bad.join("\n")
        ));
    }
    Ok(parsed
        .into_iter()
        .flatten()
        .filter(|w| w.wake <= now)
        .map(|w| {
            format!(
                "line {}: wake time {} has already passed; run its check now",
                w.line,
                w.wake.to_rfc3339()
            )
        })
        .collect())
}

/// What `lane-restart waits` prints: one block per wait, soonest first, with the time left, and
/// every malformed line named. Empty text for a HANDOFF with no wait lines.
pub fn report(text: &str, now: DateTime<Utc>) -> String {
    let mut waits = Vec::new();
    let mut out = String::new();
    for r in parse(text) {
        match r {
            Ok(w) => waits.push(w),
            Err(m) => out.push_str(&format!("line {}: MALFORMED: {}\n", m.line, m.why)),
        }
    }
    waits.sort_by_key(|w| w.wake);
    for w in waits {
        let mins = (w.wake - now).num_minutes();
        let when = if mins >= 0 {
            format!("in {mins} min")
        } else {
            format!("{} min ago", -mins)
        };
        out.push_str(&format!(
            "line {}: {}\n  check: {}\n  wake:  {} ({when}): arm a timer for this time (ScheduleWakeup or a cron), and run the check on waking\n",
            w.line,
            w.event,
            w.check,
            w.wake.to_rfc3339()
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    const NOW: &str = "2026-11-01T12:00:00Z";
    const GOOD: &str =
        "WAIT: agentlife#220 merges | check: gh pr view 220 --json merged --jq .merged | wake: 2026-11-01T12:30:00Z";

    #[test]
    fn a_complete_wait_parses_into_its_three_parts() {
        let w = parse(GOOD).remove(0).unwrap();
        assert_eq!(w.line, 1);
        assert_eq!(w.event, "agentlife#220 merges");
        assert_eq!(w.check, "gh pr view 220 --json merged --jq .merged");
        assert_eq!(w.wake, t("2026-11-01T12:30:00Z"));
    }

    #[test]
    fn list_markers_and_indent_are_allowed_but_mid_sentence_mentions_are_not_waits() {
        let text = format!(
            "- {GOOD}\n   * {GOOD}\nThe note `WAIT:` is explained here.\nno WAIT: here either\n"
        );
        let got = parse(&text);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got.iter().all(|r| r.is_ok()));
        assert_eq!(got[1].as_ref().unwrap().line, 2);
    }

    #[test]
    fn each_missing_or_wrong_part_is_named() {
        let cases = [
            ("WAIT: | check: x | wake: 2026-11-01T12:30:00Z", "no event"),
            ("WAIT: e | wake: 2026-11-01T12:30:00Z", "no `check:"),
            ("WAIT: e | check: x", "no `wake:"),
            (
                "WAIT: e | check:  | wake: 2026-11-01T12:30:00Z",
                "no `check:",
            ),
            (
                "WAIT: e | check: x | wake: in 20 minutes",
                "not an absolute RFC3339",
            ),
            (
                "WAIT: e | check: x | wake: 2026-11-01 12:30",
                "not an absolute RFC3339",
            ),
            (
                "WAIT: e | check: x | wake: 2026-11-01T12:30:00Z | then: y",
                "unknown part",
            ),
        ];
        for (line, needle) in cases {
            let err = parse(line).remove(0).unwrap_err();
            assert!(err.why.contains(needle), "{line}: {}", err.why);
        }
    }

    #[test]
    fn a_non_utc_offset_is_converted_not_rejected() {
        let w = parse("WAIT: e | check: x | wake: 2026-11-01T14:30:00+02:00")
            .remove(0)
            .unwrap();
        assert_eq!(w.wake, t("2026-11-01T12:30:00Z"));
    }

    #[test]
    fn refusal_lists_every_malformed_wait_and_a_handoff_with_none_passes() {
        let text =
            format!("{GOOD}\nWAIT: e | check: x\nprose\nWAIT: f | wake: 2026-11-01T12:30:00Z\n");
        let err = refusal(&text, t(NOW)).unwrap_err();
        assert!(err.contains("2 wait line(s)"), "{err}");
        assert!(err.contains("line 2:") && err.contains("line 4:"), "{err}");
        assert!(!err.contains("line 1:"), "{err}");
        assert_eq!(refusal("# plain handoff\n", t(NOW)), Ok(vec![]));
        assert_eq!(refusal(GOOD, t(NOW)), Ok(vec![]));
    }

    #[test]
    fn a_wake_in_the_past_is_a_warning_not_a_refusal() {
        let w = refusal(GOOD, t("2026-11-01T13:00:00Z")).unwrap();
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("already passed"), "{w:?}");
        // The boundary: a wake at exactly now is due.
        assert_eq!(refusal(GOOD, t("2026-11-01T12:30:00Z")).unwrap().len(), 1);
        assert!(refusal(GOOD, t("2026-11-01T12:29:59Z")).unwrap().is_empty());
    }

    #[test]
    fn the_report_orders_by_time_names_the_time_left_and_flags_bad_lines() {
        let text = "WAIT: later | check: b | wake: 2026-11-01T14:00:00Z\n\
                    WAIT: sooner | check: a | wake: 2026-11-01T11:30:00Z\n\
                    WAIT: broken\n";
        let r = report(text, t(NOW));
        let sooner = r.find("sooner").unwrap();
        let later = r.find("later").unwrap();
        assert!(sooner < later, "{r}");
        assert!(r.contains("30 min ago"), "{r}");
        assert!(r.contains("in 120 min"), "{r}");
        assert!(r.contains("line 3: MALFORMED"), "{r}");
        assert_eq!(report("nothing here\n", t(NOW)), "");
    }
}
