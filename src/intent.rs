// SPDX-License-Identifier: MIT OR Apache-2.0
//! Was this agent **closed on purpose**, or did something kill it? (DESIGN-REVISION-2 §3.)
//!
//! CireSnave: *"any lane that is intentionally closed should remain closed unless intentionally
//! reopened."* That needs a rule a machine can apply to evidence, and this is it. The hook only
//! records facts (a session started, a session ended with a reason); the interpretation lives here,
//! and **the result is written down the first time it is derived** (§3.4), so a later boot with a
//! different shutdown marker cannot reverse an earlier decision.
//!
//! The rule, over the agent's **last** session, once its process is gone:
//!
//! | last session | verdict |
//! |---|---|
//! | ended `prompt_input_exit` or `logout`, and the process did not die across a reboot | **closed on purpose** |
//! | ended `prompt_input_exit`/`logout` **before** the shutdown began (across a reboot) | **closed on purpose** |
//! | ended `prompt_input_exit`/`logout` at or **after** the shutdown began | candidate: ended at shutdown |
//! | ended `prompt_input_exit`/`logout`, across a reboot, shutdown time unknown | candidate: **cannot tell**, so restore it (the reversible error) |
//! | ended `other`, `clear`, `resume`, or any other reason | candidate: not an intentional end |
//! | never ended, died across a reboot | candidate: killed by the shutdown (a Windows update) |
//! | never ended, no reboot | candidate: crashed |
//!
//! Documented `SessionEnd` reasons (hooks docs, fetched 2026-10-06): `clear`, `resume`, `logout`,
//! `prompt_input_exit`, `other`. `clear`/`resume` continue the same process and are never closes;
//! `other` is the catch-all and is **not** treated as intentional.
//!
//! The shutdown marker comes from the System event log (Event 1074, "initiated the restart", else
//! 6006, "the event log service was stopped"), which is readable without elevation (measured
//! 2026-10-04). Where it cannot be read, an ambiguous case is a candidate, never a silent close.

use crate::identity::ProcessTable;
use crate::journal::Journal;
use crate::list::{liveness, Liveness};
use crate::registry::{AgentId, AgentRecord, ClosedHow, Intent, Registry};
use chrono::{DateTime, TimeZone, Utc};
use serde_json::json;

/// The `SessionEnd` reasons that mean a person ended the session from the prompt.
pub const INTENTIONAL_END_REASONS: &[&str] = &["prompt_input_exit", "logout"];

/// When the last shutdown began, if it can be known.
pub trait ShutdownMarker {
    /// The latest shutdown-initiated moment at or before `boot`.
    fn latest_before(&self, boot: DateTime<Utc>) -> Option<DateTime<Utc>>;
}

pub struct Facts<'a> {
    pub table: &'a dyn ProcessTable,
    /// When this boot began.
    pub boot: DateTime<Utc>,
    /// When the shutdown that preceded this boot began, if known.
    pub shutdown_start: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateWhy {
    /// Never ended and its process died across a reboot: a Windows update killed it.
    KilledByShutdown,
    /// Never ended and no reboot: it crashed or was killed.
    Crashed,
    /// A person-style end, but at or after the shutdown began.
    EndedAtShutdown { reason: String },
    /// A person-style end across a reboot whose shutdown time cannot be read.
    EndedBeforeUnknownShutdown { reason: String },
    /// Ended with a reason that is not an intentional close.
    EndReasonNotIntentional { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Its process is alive.
    Running,
    /// Already closed or lazy: not evaluated.
    NotWanted,
    /// A record with no sessions.
    NoSession,
    ClosedOnPurpose {
        reason: String,
        at: DateTime<Utc>,
    },
    Candidate(CandidateWhy),
}

fn process_started(rec: &AgentRecord) -> Option<DateTime<Utc>> {
    let last = rec.sessions.last()?;
    Some(
        last.process_start_secs
            .and_then(|s| Utc.timestamp_opt(s as i64, 0).single())
            .unwrap_or(last.started_at),
    )
}

/// Applies the rule to one agent. Pure: everything it needs is in `facts`.
pub fn judge(rec: &AgentRecord, facts: &Facts) -> Verdict {
    if !matches!(rec.intent, Intent::Wanted) {
        return Verdict::NotWanted;
    }
    let Some(last) = rec.sessions.last() else {
        return Verdict::NoSession;
    };
    if liveness(rec, facts.table).0 != Liveness::Stopped {
        return Verdict::Running;
    }
    let across_reboot = process_started(rec).is_some_and(|s| s < facts.boot);
    match (&last.ended_at, last.end_reason.as_deref()) {
        (Some(at), Some(reason)) if INTENTIONAL_END_REASONS.contains(&reason) => {
            let reason = reason.to_string();
            if !across_reboot {
                return Verdict::ClosedOnPurpose { reason, at: *at };
            }
            match facts.shutdown_start {
                Some(start) if *at < start => Verdict::ClosedOnPurpose { reason, at: *at },
                Some(_) => Verdict::Candidate(CandidateWhy::EndedAtShutdown { reason }),
                None => Verdict::Candidate(CandidateWhy::EndedBeforeUnknownShutdown { reason }),
            }
        }
        (Some(_), reason) => Verdict::Candidate(CandidateWhy::EndReasonNotIntentional {
            reason: reason.unwrap_or("none").to_string(),
        }),
        (None, _) => Verdict::Candidate(if across_reboot {
            CandidateWhy::KilledByShutdown
        } else {
            CandidateWhy::Crashed
        }),
    }
}

impl Verdict {
    /// One line for a person.
    pub fn describe(&self) -> String {
        match self {
            Verdict::Running => "running".to_string(),
            Verdict::NotWanted => "not evaluated (already closed or lazy)".to_string(),
            Verdict::NoSession => "no session recorded".to_string(),
            Verdict::ClosedOnPurpose { reason, at } => {
                format!("closed on purpose ({reason}, {})", at.format("%Y-%m-%d %H:%MZ"))
            }
            Verdict::Candidate(why) => match why {
                CandidateWhy::KilledByShutdown => {
                    "killed by the shutdown; a restore candidate".to_string()
                }
                CandidateWhy::Crashed => {
                    "died with no end recorded and no reboot (a crash?); a restore candidate".to_string()
                }
                CandidateWhy::EndedAtShutdown { reason } => format!(
                    "ended ({reason}) at or after the shutdown began; a restore candidate"
                ),
                CandidateWhy::EndedBeforeUnknownShutdown { reason } => format!(
                    "ended ({reason}) across a reboot whose shutdown time cannot be read: cannot tell, so a restore candidate"
                ),
                CandidateWhy::EndReasonNotIntentional { reason } => format!(
                    "ended with reason {reason}, which is not an intentional close; a restore candidate"
                ),
            },
        }
    }
}

/// One agent's verdict from a reconcile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciled {
    pub agent_id: AgentId,
    pub name: Option<String>,
    pub verdict: Verdict,
    /// True when this run wrote a `Closed` intent for it.
    pub persisted: bool,
}

/// Judges every agent and, unless `dry_run`, **persists** each "closed on purpose" verdict as
/// `Intent::Closed { Exited }` with a journal entry that carries its evidence (§3.4).
/// Idempotent: a closed agent is `NotWanted` the next time.
pub fn reconcile(
    registry: &Registry,
    journal: &Journal,
    facts: &Facts,
    dry_run: bool,
) -> Result<Vec<Reconciled>, String> {
    let listing = registry.list().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for rec in &listing.records {
        let verdict = judge(rec, facts);
        let mut persisted = false;
        if let Verdict::ClosedOnPurpose { reason, at } = &verdict {
            if !dry_run {
                let session = rec.sessions.last().map(|s| s.session_id.clone());
                let updated = registry
                    .update(&rec.agent_id, |r| {
                        // Re-check under the lock: still wanted, and still the same last session.
                        if matches!(r.intent, Intent::Wanted)
                            && r.sessions.last().map(|s| s.session_id.clone()) == session
                        {
                            r.intent = Intent::Closed {
                                how: ClosedHow::Exited,
                                by: "inferred".to_string(),
                                at: *at,
                            };
                            persisted = true;
                        }
                    })
                    .map_err(|e| e.to_string())?;
                if persisted {
                    let _ = journal.append(
                        "closed-inferred",
                        Some(updated.agent_id.as_str()),
                        json!({
                            "how": "exited",
                            "reason": reason,
                            "ended_at": at,
                            "shutdown_start": facts.shutdown_start,
                            "boot": facts.boot,
                        }),
                    );
                }
            }
        }
        out.push(Reconciled {
            agent_id: rec.agent_id.clone(),
            name: rec.name.clone(),
            verdict,
            persisted,
        });
    }
    Ok(out)
}

// ---- the real shutdown marker: the System event log ------------------------------------------

/// Reads Event 1074 / 6006 from the System log through `wevtutil` (Windows only; elsewhere there is
/// no marker and ambiguous cases become candidates).
pub struct EventLogMarker;

/// `(event id, time)` for every `<Event>` in `wevtutil qe ... /f:xml` output.
pub fn parse_events(xml: &str) -> Vec<(u32, DateTime<Utc>)> {
    xml.split("<Event ")
        .skip(1)
        .filter_map(|chunk| {
            let id_start = chunk.find("<EventID")?;
            let after = &chunk[id_start..];
            let gt = after.find('>')?;
            let end = after.find("</EventID>")?;
            let id: u32 = after.get(gt + 1..end)?.trim().parse().ok()?;
            let t = chunk.find("SystemTime='")? + "SystemTime='".len();
            let t_end = chunk[t..].find('\'')? + t;
            let time = DateTime::parse_from_rfc3339(&chunk[t..t_end]).ok()?;
            Some((id, time.with_timezone(&Utc)))
        })
        .collect()
}

/// The latest 1074 at or before `boot`, else the latest 6006 at or before it.
pub fn pick_shutdown(
    events: &[(u32, DateTime<Utc>)],
    boot: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let latest = |id: u32| {
        events
            .iter()
            .filter(|(i, t)| *i == id && *t <= boot)
            .map(|(_, t)| *t)
            .max()
    };
    latest(1074).or_else(|| latest(6006))
}

impl ShutdownMarker for EventLogMarker {
    fn latest_before(&self, boot: DateTime<Utc>) -> Option<DateTime<Utc>> {
        #[cfg(windows)]
        {
            let out = std::process::Command::new("wevtutil")
                .args([
                    "qe",
                    "System",
                    "/q:*[System[(EventID=1074 or EventID=6006)]]",
                    "/c:50",
                    "/rd:true",
                    "/f:xml",
                ])
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            pick_shutdown(&parse_events(&String::from_utf8_lossy(&out.stdout)), boot)
        }
        #[cfg(not(windows))]
        {
            let _ = boot;
            None
        }
    }
}

/// When this boot began, from the OS.
pub fn boot_time() -> DateTime<Utc> {
    Utc.timestamp_opt(sysinfo::System::boot_time() as i64, 0)
        .single()
        .unwrap_or_else(Utc::now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::SystemClock;
    use crate::identity::ProcessIdentity;
    use crate::registry::Session;
    use std::sync::Arc;

    fn t(day: u32, h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, day, h, m, 0).unwrap()
    }

    struct Alive(Vec<(u32, u64)>);
    impl ProcessTable for Alive {
        fn identity_of(&self, pid: u32) -> Option<ProcessIdentity> {
            self.0
                .iter()
                .find(|(p, _)| *p == pid)
                .map(|(p, s)| ProcessIdentity {
                    pid: *p,
                    start_secs: *s,
                    exe: None,
                })
        }
    }

    /// One agent with one session. `started` is the process start; `ended` is `(when, reason)`.
    fn agent(
        name: &str,
        started: DateTime<Utc>,
        ended: Option<(DateTime<Utc>, &str)>,
    ) -> AgentRecord {
        let mut r = AgentRecord::new(AgentId::new(name).unwrap(), "C:/Projects/x", started);
        r.name = Some(name.to_string());
        r.sessions.push(Session {
            session_id: format!("s-{name}"),
            pid: 100,
            process_start_secs: Some(started.timestamp() as u64),
            started_at: started,
            ended_at: ended.map(|(at, _)| at),
            end_reason: ended.map(|(_, r)| r.to_string()),
        });
        r
    }

    const BOOT_DAY: u32 = 7;
    fn facts<'a>(table: &'a dyn ProcessTable, shutdown: Option<DateTime<Utc>>) -> Facts<'a> {
        // Boot at 7th 12:00; the shutdown before it began at 6th 23:00.
        Facts {
            table,
            boot: t(BOOT_DAY, 12, 0),
            shutdown_start: shutdown,
        }
    }

    /// The whole rule as one table, asserted BY AGENT NAME (not by count).
    #[test]
    fn the_intent_rule_table() {
        let dead = Alive(vec![]);
        let shutdown = Some(t(6, 23, 0));
        let before_boot = t(5, 9, 0); // a process that started before the reboot
        let this_boot = t(BOOT_DAY, 13, 0); // a process that started after it

        // (name, record, shutdown marker, expected)
        let cases: Vec<(&str, AgentRecord, Option<DateTime<Utc>>, Verdict)> = vec![
            // /exit before the shutdown began: closed on purpose.
            (
                "exit-before-shutdown",
                agent(
                    "exit-before-shutdown",
                    before_boot,
                    Some((t(6, 18, 0), "prompt_input_exit")),
                ),
                shutdown,
                Verdict::ClosedOnPurpose {
                    reason: "prompt_input_exit".into(),
                    at: t(6, 18, 0),
                },
            ),
            (
                "logout-before-shutdown",
                agent(
                    "logout-before-shutdown",
                    before_boot,
                    Some((t(6, 18, 0), "logout")),
                ),
                shutdown,
                Verdict::ClosedOnPurpose {
                    reason: "logout".into(),
                    at: t(6, 18, 0),
                },
            ),
            // /exit after the shutdown began: Windows closed it, not a person.
            (
                "exit-after-shutdown-began",
                agent(
                    "exit-after-shutdown-began",
                    before_boot,
                    Some((t(6, 23, 1), "prompt_input_exit")),
                ),
                shutdown,
                Verdict::Candidate(CandidateWhy::EndedAtShutdown {
                    reason: "prompt_input_exit".into(),
                }),
            ),
            // Exactly at the shutdown's start: not "before", so not intentional.
            (
                "exit-at-the-same-instant",
                agent(
                    "exit-at-the-same-instant",
                    before_boot,
                    Some((t(6, 23, 0), "prompt_input_exit")),
                ),
                shutdown,
                Verdict::Candidate(CandidateWhy::EndedAtShutdown {
                    reason: "prompt_input_exit".into(),
                }),
            ),
            // Across a reboot with no readable shutdown time: cannot tell, so restore it.
            (
                "exit-unknown-shutdown",
                agent(
                    "exit-unknown-shutdown",
                    before_boot,
                    Some((t(6, 18, 0), "prompt_input_exit")),
                ),
                None,
                Verdict::Candidate(CandidateWhy::EndedBeforeUnknownShutdown {
                    reason: "prompt_input_exit".into(),
                }),
            ),
            // No reboot: a person exited during this boot.
            (
                "exit-this-boot",
                agent(
                    "exit-this-boot",
                    this_boot,
                    Some((t(BOOT_DAY, 14, 0), "prompt_input_exit")),
                ),
                None,
                Verdict::ClosedOnPurpose {
                    reason: "prompt_input_exit".into(),
                    at: t(BOOT_DAY, 14, 0),
                },
            ),
            // Reasons that are not intentional closes.
            (
                "ended-other",
                agent("ended-other", before_boot, Some((t(6, 18, 0), "other"))),
                shutdown,
                Verdict::Candidate(CandidateWhy::EndReasonNotIntentional {
                    reason: "other".into(),
                }),
            ),
            (
                "ended-clear",
                agent("ended-clear", before_boot, Some((t(6, 18, 0), "clear"))),
                shutdown,
                Verdict::Candidate(CandidateWhy::EndReasonNotIntentional {
                    reason: "clear".into(),
                }),
            ),
            (
                "ended-resume",
                agent("ended-resume", before_boot, Some((t(6, 18, 0), "resume"))),
                shutdown,
                Verdict::Candidate(CandidateWhy::EndReasonNotIntentional {
                    reason: "resume".into(),
                }),
            ),
            // Never ended.
            (
                "killed-by-the-update",
                agent("killed-by-the-update", before_boot, None),
                shutdown,
                Verdict::Candidate(CandidateWhy::KilledByShutdown),
            ),
            (
                "crashed-this-boot",
                agent("crashed-this-boot", this_boot, None),
                None,
                Verdict::Candidate(CandidateWhy::Crashed),
            ),
        ];
        let mut seen = std::collections::BTreeSet::new();
        for (name, rec, marker, want) in cases {
            let got = judge(&rec, &facts(&dead, marker));
            assert_eq!(got, want, "{name}");
            assert!(seen.insert(name), "{name} listed twice");
        }
        assert_eq!(seen.len(), 11);
    }

    #[test]
    fn a_running_agent_is_running_whatever_it_once_did() {
        let started = t(5, 9, 0);
        let rec = agent("alive", started, None);
        let table = Alive(vec![(100, started.timestamp() as u64)]);
        assert_eq!(
            judge(&rec, &facts(&table, Some(t(6, 23, 0)))),
            Verdict::Running
        );
    }

    #[test]
    fn a_recycled_pid_does_not_make_a_dead_agent_running() {
        let started = t(5, 9, 0);
        let rec = agent("recycled", started, None);
        // Something else now holds pid 100, started later.
        let table = Alive(vec![(100, t(7, 13, 0).timestamp() as u64)]);
        assert!(matches!(
            judge(&rec, &facts(&table, Some(t(6, 23, 0)))),
            Verdict::Candidate(CandidateWhy::KilledByShutdown)
        ));
    }

    #[test]
    fn closed_and_lazy_agents_are_not_evaluated_and_an_empty_record_is_reported() {
        let dead = Alive(vec![]);
        let mut parked = agent("parked", t(5, 9, 0), None);
        parked.intent = Intent::Closed {
            how: ClosedHow::Parked,
            by: "person".into(),
            at: t(6, 1, 0),
        };
        assert_eq!(judge(&parked, &facts(&dead, None)), Verdict::NotWanted);
        let mut lazy = agent("lazy", t(5, 9, 0), None);
        lazy.intent = Intent::Lazy;
        assert_eq!(judge(&lazy, &facts(&dead, None)), Verdict::NotWanted);
        let empty = AgentRecord::new(AgentId::new("empty").unwrap(), "C:/x", t(5, 9, 0));
        assert_eq!(judge(&empty, &facts(&dead, None)), Verdict::NoSession);
    }

    struct Rig {
        _d: tempfile::TempDir,
        registry: Registry,
        journal: Journal,
    }

    fn rig() -> Rig {
        let d = tempfile::tempdir().unwrap();
        Rig {
            registry: Registry::new(d.path().join("agents")),
            journal: Journal::new(d.path().join("journal"), Arc::new(SystemClock)),
            _d: d,
        }
    }

    #[test]
    fn reconcile_persists_a_closed_on_purpose_verdict_once_and_only_that_one() {
        let r = rig();
        let dead = Alive(vec![]);
        let f = facts(&dead, Some(t(6, 23, 0)));
        for (name, ended) in [
            ("closed-one", Some((t(6, 18, 0), "prompt_input_exit"))),
            ("killed-one", None),
        ] {
            r.registry.create(&agent(name, t(5, 9, 0), ended)).unwrap();
        }
        let out = reconcile(&r.registry, &r.journal, &f, false).unwrap();
        let by_name = |n: &str| out.iter().find(|x| x.name.as_deref() == Some(n)).unwrap();
        assert!(by_name("closed-one").persisted);
        assert!(
            !by_name("killed-one").persisted,
            "a candidate is derived, never written"
        );

        let closed = r
            .registry
            .get(&AgentId::new("closed-one").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(
            closed.intent,
            Intent::Closed {
                how: ClosedHow::Exited,
                by: "inferred".into(),
                at: t(6, 18, 0)
            }
        );
        assert_eq!(
            r.registry
                .get(&AgentId::new("killed-one").unwrap())
                .unwrap()
                .unwrap()
                .intent,
            Intent::Wanted
        );
        let entries = r.journal.read_all().unwrap().entries;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, "closed-inferred");
        assert_eq!(entries[0].data["reason"], "prompt_input_exit");

        // Idempotent: a second run changes nothing and writes nothing.
        let again = reconcile(&r.registry, &r.journal, &f, false).unwrap();
        assert!(again.iter().all(|x| !x.persisted));
        assert_eq!(r.journal.read_all().unwrap().entries.len(), 1);
        assert_eq!(
            again
                .iter()
                .find(|x| x.name.as_deref() == Some("closed-one"))
                .unwrap()
                .verdict,
            Verdict::NotWanted
        );
    }

    #[test]
    fn a_later_boot_with_a_different_marker_cannot_reverse_a_persisted_decision() {
        // §3.4: derived once, written down. With no marker at all the same record would now be a
        // candidate; but it was already closed, and stays closed.
        let r = rig();
        let dead = Alive(vec![]);
        r.registry
            .create(&agent(
                "decided",
                t(5, 9, 0),
                Some((t(6, 18, 0), "prompt_input_exit")),
            ))
            .unwrap();
        reconcile(
            &r.registry,
            &r.journal,
            &facts(&dead, Some(t(6, 23, 0))),
            false,
        )
        .unwrap();
        let out = reconcile(&r.registry, &r.journal, &facts(&dead, None), false).unwrap();
        assert_eq!(out[0].verdict, Verdict::NotWanted);
    }

    #[test]
    fn a_dry_run_reports_the_verdict_and_writes_nothing() {
        let r = rig();
        let dead = Alive(vec![]);
        r.registry
            .create(&agent(
                "dry",
                t(5, 9, 0),
                Some((t(6, 18, 0), "prompt_input_exit")),
            ))
            .unwrap();
        let out = reconcile(
            &r.registry,
            &r.journal,
            &facts(&dead, Some(t(6, 23, 0))),
            true,
        )
        .unwrap();
        assert!(matches!(out[0].verdict, Verdict::ClosedOnPurpose { .. }));
        assert!(!out[0].persisted);
        assert_eq!(
            r.registry
                .get(&AgentId::new("dry").unwrap())
                .unwrap()
                .unwrap()
                .intent,
            Intent::Wanted
        );
        assert!(r.journal.read_all().unwrap().entries.is_empty());
    }

    // -- the event-log reader --------------------------------------------------------------

    const SAMPLE: &str =
        "<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'><System>\
<Provider Name='User32'/><EventID Qualifiers='32768'>1074</EventID><Level>4</Level>\
<TimeCreated SystemTime='2026-09-16T08:48:37.1234567Z'/></System></Event>\
<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'><System>\
<Provider Name='EventLog'/><EventID Qualifiers='32768'>6006</EventID>\
<TimeCreated SystemTime='2026-09-16T08:48:38.0000000Z'/></System></Event>\
<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'><System>\
<EventID>1074</EventID><TimeCreated SystemTime='2026-10-01T03:00:00.5Z'/></System></Event>";

    #[test]
    fn the_event_xml_is_parsed_into_ids_and_times() {
        let ev = parse_events(SAMPLE);
        assert_eq!(ev.len(), 3);
        assert_eq!(ev[0].0, 1074);
        assert_eq!(
            ev[0].1.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-09-16 08:48:37"
        );
        assert_eq!(ev[1].0, 6006);
        assert_eq!(ev[2].0, 1074);
        assert!(parse_events("").is_empty());
        assert!(parse_events("<Event garbage").is_empty());
    }

    #[test]
    fn the_shutdown_is_the_latest_1074_before_the_boot_else_the_latest_6006() {
        let ev = parse_events(SAMPLE);
        let boot = |d: (u32, u32), h: u32| Utc.with_ymd_and_hms(2026, d.0, d.1, h, 0, 0).unwrap();
        // Boot on 17 Sept: the 16 Sept 1074 is the latest before it.
        assert_eq!(
            pick_shutdown(&ev, boot((9, 17), 0))
                .unwrap()
                .format("%H:%M:%S")
                .to_string(),
            "08:48:37"
        );
        // Boot on 2 Oct: the 1 Oct 1074 wins over the older one.
        assert_eq!(
            pick_shutdown(&ev, boot((10, 2), 0))
                .unwrap()
                .format("%m-%d")
                .to_string(),
            "10-01"
        );
        // An event AFTER the boot is not a shutdown that preceded it.
        assert!(pick_shutdown(&ev, boot((9, 1), 0)).is_none());
        // With only a 6006 available, it is used.
        let only_6006 = vec![(6006, Utc.with_ymd_and_hms(2026, 9, 16, 8, 48, 38).unwrap())];
        assert!(pick_shutdown(&only_6006, boot((9, 17), 0)).is_some());
        assert!(pick_shutdown(&[], boot((9, 17), 0)).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn the_real_event_log_can_be_read_without_elevation() {
        // On a machine that has restarted, 1074 or 6006 exists before the boot. A CI runner may
        // not have one, so only the contract is asserted: no panic, and never a time after boot.
        let boot = boot_time();
        if let Some(t) = EventLogMarker.latest_before(boot) {
            assert!(t <= boot);
        }
    }

    #[test]
    fn every_verdict_has_its_own_plain_sentence() {
        let vs = [
            Verdict::Running,
            Verdict::NotWanted,
            Verdict::NoSession,
            Verdict::ClosedOnPurpose {
                reason: "logout".into(),
                at: t(6, 18, 0),
            },
            Verdict::Candidate(CandidateWhy::KilledByShutdown),
            Verdict::Candidate(CandidateWhy::Crashed),
            Verdict::Candidate(CandidateWhy::EndedAtShutdown {
                reason: "logout".into(),
            }),
            Verdict::Candidate(CandidateWhy::EndedBeforeUnknownShutdown {
                reason: "logout".into(),
            }),
            Verdict::Candidate(CandidateWhy::EndReasonNotIntentional {
                reason: "other".into(),
            }),
        ];
        let texts: std::collections::BTreeSet<String> = vs.iter().map(Verdict::describe).collect();
        assert_eq!(
            texts.len(),
            vs.len(),
            "no two verdicts read the same: {texts:?}"
        );
        assert!(texts
            .iter()
            .any(|x| x.contains("closed on purpose (logout, 2026-10-06 18:00Z)")));
    }

    #[test]
    fn the_boot_time_is_in_the_past() {
        assert!(boot_time() < Utc::now());
    }
}
