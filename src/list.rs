// SPDX-License-Identifier: MIT OR Apache-2.0
//! `agentlife list`: the registry joined with the process table. Read-only; no consent.
//!
//! Liveness is **derived** from the process table (pid + start time) and never stored as truth
//! (DESIGN-REVISION-2 §2). An agent whose last session has no recorded start time cannot be
//! verified, and is shown as `running?` rather than guessed.

use crate::config::Config;
use crate::identity::{self, Match, ProcessIdentity, ProcessTable};
use crate::marks::{is_waiting, pin_kind, PinKind};
use crate::registry::{AgentRecord, ClosedHow, Intent};
use chrono::{DateTime, Utc};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    Running,
    /// Alive, but the record has no start time to prove it is the same process.
    Unverified,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Row {
    pub agent_id: String,
    pub name: Option<String>,
    pub intent: String,
    pub liveness: Liveness,
    pub pid: Option<u32>,
    pub permission_mode: Option<String>,
    pub model: Option<String>,
    pub origin: String,
    pub launch_cwd: String,
    /// `rule` (the PM, by the visible rule) or `explicit` (pinned by hand); never lazy-stopped.
    pub pinned: Option<String>,
    /// A waiting-on-the-user mark that has not expired.
    pub waiting: bool,
    pub sessions: usize,
    /// The latest of the last session's start and end.
    pub last_activity: Option<DateTime<Utc>>,
}

fn intent_label(i: &Intent) -> &'static str {
    match i {
        Intent::Wanted => "wanted",
        Intent::Lazy => "lazy",
        Intent::Closed {
            how: ClosedHow::Parked,
            ..
        } => "parked",
        Intent::Closed {
            how: ClosedHow::Exited,
            ..
        } => "exited",
    }
}

pub fn liveness(rec: &AgentRecord, table: &dyn ProcessTable) -> (Liveness, Option<u32>) {
    let Some(last) = rec.sessions.last() else {
        return (Liveness::Stopped, None);
    };
    if last.ended_at.is_some() {
        return (Liveness::Stopped, Some(last.pid));
    }
    match last.process_start_secs {
        Some(start) => {
            let m = identity::check(
                table,
                &ProcessIdentity {
                    pid: last.pid,
                    start_secs: start,
                    exe: None,
                },
            );
            if m == Match::Same {
                (Liveness::Running, Some(last.pid))
            } else {
                (Liveness::Stopped, Some(last.pid))
            }
        }
        None => {
            if table.identity_of(last.pid).is_some() {
                (Liveness::Unverified, Some(last.pid))
            } else {
                (Liveness::Stopped, Some(last.pid))
            }
        }
    }
}

pub fn rows(
    records: &[AgentRecord],
    table: &dyn ProcessTable,
    include_closed: bool,
    cfg: &Config,
    now: DateTime<Utc>,
) -> Vec<Row> {
    let mut out: Vec<Row> = records
        .iter()
        .filter(|r| include_closed || !matches!(r.intent, Intent::Closed { .. }))
        .map(|r| {
            let (live, pid) = liveness(r, table);
            Row {
                agent_id: r.agent_id.to_string(),
                name: r.name.clone(),
                intent: intent_label(&r.intent).to_string(),
                liveness: live,
                pid,
                permission_mode: r.permission_mode.clone(),
                model: r.model.clone(),
                origin: format!("{:?}", r.origin).to_lowercase(),
                launch_cwd: r.launch_cwd.clone(),
                pinned: pin_kind(r, cfg).map(|k| match k {
                    PinKind::Rule => "rule".to_string(),
                    PinKind::Explicit => "explicit".to_string(),
                }),
                waiting: is_waiting(r, now, cfg.waiting_mark_ttl_hours),
                sessions: r.sessions.len(),
                last_activity: r
                    .sessions
                    .last()
                    .map(|s| s.ended_at.unwrap_or(s.started_at)),
            }
        })
        .collect();
    out.sort_by(|a, b| {
        a.name
            .as_deref()
            .unwrap_or("~")
            .to_lowercase()
            .cmp(&b.name.as_deref().unwrap_or("~").to_lowercase())
            .then_with(|| a.agent_id.cmp(&b.agent_id))
    });
    out
}

fn live_label(l: Liveness) -> &'static str {
    match l {
        Liveness::Running => "running",
        Liveness::Unverified => "running?",
        Liveness::Stopped => "stopped",
    }
}

pub fn render_text(rows: &[Row]) -> String {
    let header = [
        "AGENT",
        "NAME",
        "INTENT",
        "STATE",
        "PIN",
        "WAIT",
        "PID",
        "MODE",
        "ORIGIN",
        "LAST ACTIVITY",
        "CWD",
    ];
    let body: Vec<[String; 11]> = rows
        .iter()
        .map(|r| {
            [
                r.agent_id.clone(),
                r.name.clone().unwrap_or_else(|| "-".into()),
                r.intent.clone(),
                live_label(r.liveness).to_string(),
                r.pinned.clone().unwrap_or_else(|| "-".into()),
                if r.waiting { "yes".into() } else { "-".into() },
                r.pid.map_or("-".into(), |p| p.to_string()),
                r.permission_mode.clone().unwrap_or_else(|| "-".into()),
                r.origin.clone(),
                r.last_activity
                    .map_or("-".into(), |t| t.format("%Y-%m-%d %H:%MZ").to_string()),
                r.launch_cwd.clone(),
            ]
        })
        .collect();
    let mut widths: Vec<usize> = header.iter().map(|h| h.len()).collect();
    for row in &body {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let line = |cells: Vec<&str>| {
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c:<w$}", w = widths[i]))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    let mut out = line(header.to_vec());
    out.push('\n');
    for row in &body {
        out.push_str(&line(row.iter().map(String::as_str).collect()));
        out.push('\n');
    }
    let running = rows
        .iter()
        .filter(|r| r.liveness == Liveness::Running)
        .count();
    out.push_str(&format!("{} agents, {running} running\n", rows.len()));
    out
}

pub fn render_json(rows: &[Row]) -> String {
    serde_json::to_string_pretty(rows).unwrap_or_else(|_| "[]".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{AgentId, Origin, Session};
    use chrono::TimeZone;

    fn t(min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 7, 1, min, 0).unwrap()
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

    fn rec(id: &str, name: Option<&str>, session: Option<(u32, Option<u64>, bool)>) -> AgentRecord {
        let mut r = AgentRecord::new(AgentId::new(id).unwrap(), "C:/Projects/x", t(0));
        r.name = name.map(String::from);
        r.origin = Origin::Hand;
        if let Some((pid, start, ended)) = session {
            r.sessions.push(Session {
                session_id: format!("s-{id}"),
                pid,
                process_start_secs: start,
                started_at: t(1),
                ended_at: ended.then(|| t(9)),
                end_reason: ended.then(|| "other".to_string()),
            });
        }
        r
    }

    #[test]
    fn liveness_is_pid_plus_start_time_never_the_pid_alone() {
        let table = Alive(vec![(10, 1000), (20, 9999)]);
        // Running: alive, same start time.
        assert_eq!(
            liveness(&rec("a", None, Some((10, Some(1000), false))), &table).0,
            Liveness::Running
        );
        // A recycled pid: alive, but a different process now.
        assert_eq!(
            liveness(&rec("b", None, Some((20, Some(1000), false))), &table).0,
            Liveness::Stopped
        );
        // Gone.
        assert_eq!(
            liveness(&rec("c", None, Some((30, Some(1000), false))), &table).0,
            Liveness::Stopped
        );
        // Ended by its own SessionEnd, even if the pid happens to be live again.
        assert_eq!(
            liveness(&rec("d", None, Some((10, Some(1000), true))), &table).0,
            Liveness::Stopped
        );
        // No start time: alive pid is only "unverified"; a dead pid is stopped.
        assert_eq!(
            liveness(&rec("e", None, Some((10, None, false))), &table).0,
            Liveness::Unverified
        );
        assert_eq!(
            liveness(&rec("f", None, Some((30, None, false))), &table).0,
            Liveness::Stopped
        );
        // No session at all.
        assert_eq!(liveness(&rec("g", None, None), &table).0, Liveness::Stopped);
    }

    #[test]
    fn closed_agents_are_hidden_unless_asked_for_and_the_order_is_by_name() {
        let table = Alive(vec![]);
        let mut parked = rec("p", Some("zeta"), None);
        parked.intent = Intent::Closed {
            how: ClosedHow::Parked,
            by: "person".into(),
            at: t(2),
        };
        let records = vec![
            rec("b", Some("beta"), None),
            parked,
            rec("a", Some("Alpha"), None),
            rec("n", None, None),
        ];
        let names = |rows: Vec<Row>| rows.into_iter().map(|r| r.agent_id).collect::<Vec<_>>();
        assert_eq!(
            names(rows(&records, &table, false, &Config::default(), t(9))),
            ["a", "b", "n"],
            "nameless last, closed hidden"
        );
        assert_eq!(
            names(rows(&records, &table, true, &Config::default(), t(9))),
            ["a", "b", "p", "n"]
        );
        let all = rows(&records, &table, true, &Config::default(), t(9));
        assert_eq!(
            all.iter().find(|r| r.agent_id == "p").unwrap().intent,
            "parked"
        );
    }

    #[test]
    fn the_text_table_has_a_header_aligned_columns_and_a_count() {
        let table = Alive(vec![(10, 1000)]);
        let records = vec![
            rec("a1", Some("synapse"), Some((10, Some(1000), false))),
            rec("a2", Some("overmind"), Some((30, Some(1), false))),
        ];
        let out = render_text(&rows(&records, &table, false, &Config::default(), t(9)));
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("AGENT"), "{out}");
        assert!(
            lines[1].contains("overmind") && lines[1].contains("stopped"),
            "{out}"
        );
        assert!(
            lines[2].contains("synapse") && lines[2].contains("running"),
            "{out}"
        );
        assert_eq!(lines.last().unwrap(), &"2 agents, 1 running");
        // Alignment: every data line has the STATE column at the same offset as the header.
        let col = lines[0].find("STATE").unwrap();
        assert_eq!(&lines[1][col..col + 7], "stopped");
        assert_eq!(&lines[2][col..col + 7], "running");
    }

    #[test]
    fn json_output_is_a_parseable_array_with_the_liveness_spelled_out() {
        let table = Alive(vec![(10, 1000)]);
        let records = vec![rec("a1", Some("synapse"), Some((10, Some(1000), false)))];
        let v: serde_json::Value = serde_json::from_str(&render_json(&rows(
            &records,
            &table,
            false,
            &Config::default(),
            t(9),
        )))
        .unwrap();
        assert_eq!(v[0]["agent_id"], "a1");
        assert_eq!(v[0]["liveness"], "running");
        assert_eq!(v[0]["intent"], "wanted");
        assert_eq!(render_json(&[]), "[]");
    }
}
