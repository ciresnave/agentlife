// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart ledger` - a read-only reader of `restart.log` (RESTART-TOOL-DESIGN.md 6.3).
//!
//! The log is append-only JSON lines: one per start, stop, restart, import, retire and refusal. Who
//! restarted whom was answered by reading it by hand; this prints it filtered. It writes nothing -
//! not the log, not a cache. A line that is not an entry is **counted and reported**, never
//! skipped silently, and unknown keys are ignored (the log grows fields additively).

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One line of `restart.log`, as written by [`crate::log::append`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub timestamp: DateTime<Utc>,
    pub requested_by: String,
    pub role: String,
    pub outcome: String,
    pub acted: bool,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub caller_role: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    /// The Synapse signal a launcher start answered (0.15.13).
    #[serde(default)]
    pub signal_id: Option<String>,
    /// The `--json` result of a start, `<outcome>:<reason>` in vocabulary A (0.15.13).
    #[serde(default)]
    pub result: Option<String>,
    /// Whether a spawn was attempted: what the launcher's caps count (0.15.13).
    #[serde(default)]
    pub spawned: Option<bool>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Filter {
    pub role: Option<String>,
    pub kind: Option<String>,
    pub since: Option<DateTime<Utc>>,
    /// `Some(true)` only entries that acted, `Some(false)` only refusals.
    pub acted: Option<bool>,
    /// Keep only the newest N after the other filters.
    pub last: Option<usize>,
}

/// What reading the file produced: every parsed row in file order, and the lines that were not
/// entries (1-based line number and a short reason).
#[derive(Debug, PartialEq, Eq)]
pub struct Read {
    pub rows: Vec<Row>,
    pub unreadable: Vec<(usize, String)>,
}

pub fn parse(text: &str) -> Read {
    let mut rows = Vec::new();
    let mut unreadable = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Row>(line) {
            Ok(r) => rows.push(r),
            Err(e) => unreadable.push((i + 1, e.to_string())),
        }
    }
    Read { rows, unreadable }
}

/// Reads `path`; a missing file is an empty ledger, any other error is returned.
pub fn read(path: &Path) -> Result<Read, String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(parse(&t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Read {
            rows: Vec::new(),
            unreadable: Vec::new(),
        }),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// `2026-10-14T10:00:00Z` (RFC 3339), or a span back from `now`: `30m`, `2h`, `1d`.
pub fn parse_since(s: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(t.with_timezone(&Utc));
    }
    let (num, unit) = s.split_at(s.len().saturating_sub(1));
    let n: i64 = num
        .parse()
        .map_err(|_| format!("--since takes an RFC 3339 time or a span like 30m, 2h, 1d: {s:?}"))?;
    let span = match unit {
        "m" => Duration::minutes(n),
        "h" => Duration::hours(n),
        "d" => Duration::days(n),
        _ => {
            return Err(format!(
                "--since takes an RFC 3339 time or a span like 30m, 2h, 1d: {s:?}"
            ))
        }
    };
    Ok(now - span)
}

/// The rows that pass `f`, oldest first.
pub fn select<'a>(rows: &'a [Row], f: &Filter) -> Vec<&'a Row> {
    let mut out: Vec<&Row> = rows
        .iter()
        .filter(|r| f.role.as_ref().is_none_or(|x| &r.role == x))
        .filter(|r| {
            f.kind
                .as_ref()
                .is_none_or(|k| r.kind.as_deref().unwrap_or("restart") == k)
        })
        .filter(|r| f.since.is_none_or(|t| r.timestamp >= t))
        .filter(|r| f.acted.is_none_or(|a| r.acted == a))
        .collect();
    if let Some(n) = f.last {
        let skip = out.len().saturating_sub(n);
        out.drain(..skip);
    }
    out
}

/// `--by-lane`: per target role, how many of the shown rows acted and how many were refused, one
/// table sorted by role. Counts the rows `select` kept, so `--since` and `--kind` apply; the
/// footer says how many of the log's rows that was.
pub fn render_by_lane(shown: &[&Row], total: usize) -> String {
    let mut counts: std::collections::BTreeMap<&str, (usize, usize)> = Default::default();
    for r in shown {
        let c = counts.entry(r.role.as_str()).or_default();
        if r.acted {
            c.0 += 1;
        } else {
            c.1 += 1;
        }
    }
    let mut out = String::from("role\tacted\trefused\n");
    for (role, (acted, refused)) in &counts {
        out.push_str(&format!("{role}\t{acted}\t{refused}\n"));
    }
    out.push_str(&format!(
        "({} role(s); {} of {} entries counted)\n",
        counts.len(),
        shown.len(),
        total
    ));
    out
}

/// The text a person reads: one tab-separated row per entry, then a footer that counts what was
/// shown, what the log holds and what could not be read.
pub fn render(shown: &[&Row], total: usize, unreadable: &[(usize, String)]) -> String {
    let mut out = String::new();
    for r in shown {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\n",
            r.timestamp
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            r.role,
            r.kind.as_deref().unwrap_or("restart"),
            match r.caller_role.as_deref() {
                Some(c) if c != r.requested_by => format!("{} (caller {c})", r.requested_by),
                _ => r.requested_by.clone(),
            },
            if r.acted { "acted" } else { "refused" },
            r.outcome.replace('\n', " "),
        ));
    }
    out.push_str(&format!(
        "({} of {} entr{} shown; {} unreadable line(s){})\n",
        shown.len(),
        total,
        if total == 1 { "y" } else { "ies" },
        unreadable.len(),
        if unreadable.is_empty() {
            String::new()
        } else {
            format!(
                ": {}",
                unreadable
                    .iter()
                    .map(|(n, why)| format!("line {n} ({why})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn line(ts: &str, role: &str, kind: Option<&str>, acted: bool, outcome: &str) -> String {
        let kind = kind.map_or(String::new(), |k| format!(r#","kind":"{k}""#));
        format!(
            r#"{{"timestamp":"{ts}","requested_by":"pm","role":"{role}","outcome":"{outcome}","acted":{acted}{kind}}}"#
        )
    }

    fn sample() -> String {
        [
            line(
                "2026-10-14T08:00:00Z",
                "a",
                None,
                true,
                "killed and relaunched",
            ),
            line(
                "2026-10-14T09:00:00Z",
                "b",
                Some("start"),
                false,
                "refused: disk",
            ),
            line(
                "2026-10-14T10:00:00Z",
                "a",
                Some("import"),
                true,
                "imported",
            ),
            "not json".to_string(),
            line("2026-10-14T11:00:00Z", "a", Some("stop"), true, "stopped"),
        ]
        .join("\n")
            + "\n"
    }

    #[test]
    fn a_line_that_is_not_an_entry_is_counted_and_named_not_dropped() {
        let r = parse(&sample());
        assert_eq!(r.rows.len(), 4);
        assert_eq!(r.unreadable.len(), 1);
        assert_eq!(r.unreadable[0].0, 4, "1-based line number of the bad line");
        let text = render(
            &select(&r.rows, &Filter::default()),
            r.rows.len(),
            &r.unreadable,
        );
        assert!(
            text.contains("(4 of 4 entries shown; 1 unreadable line(s): line 4 ("),
            "{text}"
        );
    }

    #[test]
    fn filters_by_role_kind_time_and_outcome_and_an_old_line_is_a_restart() {
        let r = parse(&sample());
        let ids = |f: Filter| -> Vec<String> {
            select(&r.rows, &f)
                .iter()
                .map(|x| format!("{}:{}", x.role, x.kind.as_deref().unwrap_or("restart")))
                .collect()
        };
        assert_eq!(
            ids(Filter {
                role: Some("a".into()),
                ..Default::default()
            }),
            vec!["a:restart", "a:import", "a:stop"]
        );
        // A line written before `kind` existed is a restart.
        assert_eq!(
            ids(Filter {
                kind: Some("restart".into()),
                ..Default::default()
            }),
            vec!["a:restart"]
        );
        assert_eq!(
            ids(Filter {
                acted: Some(false),
                ..Default::default()
            }),
            vec!["b:start"]
        );
        let since = Utc.with_ymd_and_hms(2026, 10, 14, 10, 0, 0).unwrap();
        assert_eq!(
            ids(Filter {
                since: Some(since),
                ..Default::default()
            }),
            vec!["a:import", "a:stop"]
        );
        assert_eq!(
            ids(Filter {
                last: Some(2),
                ..Default::default()
            }),
            vec!["a:import", "a:stop"]
        );
    }

    #[test]
    fn since_takes_a_time_or_a_span_and_refuses_anything_else() {
        let now = Utc.with_ymd_and_hms(2026, 10, 14, 12, 0, 0).unwrap();
        assert_eq!(
            parse_since("2026-10-14T10:00:00Z", now).unwrap(),
            Utc.with_ymd_and_hms(2026, 10, 14, 10, 0, 0).unwrap()
        );
        assert_eq!(
            parse_since("30m", now).unwrap(),
            now - Duration::minutes(30)
        );
        assert_eq!(parse_since("2h", now).unwrap(), now - Duration::hours(2));
        assert_eq!(parse_since("1d", now).unwrap(), now - Duration::days(1));
        for bad in ["", "h", "5", "5x", "yesterday"] {
            assert!(parse_since(bad, now).is_err(), "{bad:?} accepted");
        }
    }

    #[test]
    fn unknown_keys_are_ignored_and_a_missing_file_is_an_empty_ledger() {
        let r = parse(
            r#"{"timestamp":"2026-10-14T08:00:00Z","requested_by":"pm","role":"a","outcome":"x","acted":true,"future_key":1}"#,
        );
        assert_eq!((r.rows.len(), r.unreadable.len()), (1, 0));
        let d = tempfile::tempdir().unwrap();
        let none = read(&d.path().join("restart.log")).unwrap();
        assert_eq!((none.rows.len(), none.unreadable.len()), (0, 0));
        assert!(
            !d.path().join("restart.log").exists(),
            "reading must not create the log"
        );
    }

    #[test]
    fn a_caller_that_differs_from_the_requester_is_shown() {
        let mut r = parse(&line(
            "2026-10-14T08:00:00Z",
            "a",
            Some("start"),
            true,
            "ok",
        ))
        .rows;
        r[0].requested_by = "unverified".into();
        r[0].caller_role = Some("pm".into());
        let text = render(&select(&r, &Filter::default()), 1, &[]);
        assert!(text.contains("unverified (caller pm)"), "{text}");
    }

    #[test]
    fn by_lane_counts_acted_and_refused_per_target_role_over_the_selected_rows() {
        let read = parse(&sample());
        let all = select(&read.rows, &Filter::default());
        assert_eq!(
            render_by_lane(&all, read.rows.len()),
            "role\tacted\trefused\na\t3\t0\nb\t0\t1\n(2 role(s); 4 of 4 entries counted)\n"
        );
        let f = Filter {
            kind: Some("start".into()),
            ..Filter::default()
        };
        let some = select(&read.rows, &f);
        let text = render_by_lane(&some, read.rows.len());
        assert!(text.contains("b\t0\t1\n"), "{text}");
        assert!(!text.contains("\na\t"), "{text}");
        assert!(text.contains("1 of 4 entries counted"), "{text}");
    }
}
