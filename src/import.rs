// SPDX-License-Identifier: MIT OR Apache-2.0
//! `agentlife import-lane-state`: seeds the registry from `.lane-state/<role>.json`
//! (RESTORE-GAP-ANALYSIS R3, R11, R12, R18) so the first restore after a reboot does not have to
//! wait for the hooks to have filled it.
//!
//! **Read-only on `.lane-state`** (OverMind's runtime directory); it writes only agentlife's own
//! registry, and only with `--write`. Everything it records is a *claim the lane made about itself*:
//! the importer filters (age, root, name, duplicates) and copies, it never raises anything. In
//! particular the permission mode is carried exactly as recorded; refusing `bypassPermissions` for a
//! non-PM is [`crate::plan`]'s job and stays there (`Reason::BypassNotPm`).
//!
//! The core ([`decide`]) is pure: states, the registry listing, the clock and a "does this
//! directory exist" probe go in; what to write and what was skipped (with why) comes out.

use crate::config::Config;
use crate::identity::{path_is_under, paths_equal};
use crate::registry::{
    AgentId, AgentRecord, ClosedHow, Intent, Origin, Registry, RegistryError, Session,
};
use chrono::{DateTime, Duration, Utc};
use lane_state::state::LaneState;
use serde::Serialize;
use std::path::Path;

/// How recent a state file must be, unless `--since` says otherwise. A filter on recent activity,
/// not on liveness: every one of these processes is dead after a reboot.
pub const DEFAULT_SINCE_HOURS: i64 = 48;

/// Sessions that are test fixtures, never lanes: a role or name containing one of these (any case).
const DENIED_SUBSTRINGS: &[&str] = &["restarttest"];

/// Who `by` names on a record this importer closed.
pub const IMPORT_ACTOR: &str = "import-lane-state";

/// Parses `48h`, `30m`, `2d` (a positive whole number and one unit).
pub fn parse_since(s: &str) -> Result<Duration, String> {
    let bad =
        || format!("{s:?} is not a duration: use a whole number and h, m or d (48h, 30m, 2d)");
    let (num, unit) = s.split_at(s.len().saturating_sub(1));
    let n: i64 = num.parse().map_err(|_| bad())?;
    if n < 1 {
        return Err(bad());
    }
    match unit {
        "m" => Ok(Duration::minutes(n)),
        "h" => Ok(Duration::hours(n)),
        "d" => Ok(Duration::days(n)),
        _ => Err(bad()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// The file is not a lane state file (`model-policy.json` is a shared setting).
    NotALaneState,
    /// Older than `--since`.
    TooOld,
    /// A test fixture's name.
    Denied,
    CwdOutsideRoot,
    CwdMissing,
    /// No recorded launch arguments, so a restore could not rebuild it.
    NoLaunchArgs,
    /// Another file with the same name and directory is newer.
    Superseded,
    /// The registry already has this name in this directory (the hook's record is fresher).
    AlreadyRegistered,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Skipped {
    /// The state file's stem.
    pub role: String,
    pub reason: SkipReason,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Imported {
    /// The state file's stem.
    pub role: String,
    pub record: AgentRecord,
}

#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    pub imports: Vec<Imported>,
    pub skipped: Vec<Skipped>,
    /// `--park` names that matched no imported agent.
    pub unmatched_park: Vec<String>,
}

pub struct Inputs<'a> {
    /// `(file stem, parsed state)`, any order.
    pub states: &'a [(String, LaneState)],
    /// Stems of files that could not be parsed as a lane state, with why.
    pub unparsed: &'a [(String, String)],
    pub existing: &'a [AgentRecord],
    pub cfg: &'a Config,
    pub now: DateTime<Utc>,
    pub since: Duration,
    /// Names (role or name, any case) to import as parked.
    pub park: &'a [String],
    pub cwd_exists: &'a dyn Fn(&str) -> bool,
}

fn name_of(role: &str, state: &LaneState) -> String {
    state
        .name
        .as_deref()
        .filter(|n| !n.is_empty())
        .unwrap_or(role)
        .to_string()
}

fn denied(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    DENIED_SUBSTRINGS.iter().any(|d| l.contains(d))
}

/// What to import and what to skip. Pure; the output order is by file stem.
pub fn decide(i: &Inputs) -> Outcome {
    let mut out = Outcome::default();
    for (stem, why) in i.unparsed {
        out.skipped.push(Skipped {
            role: stem.clone(),
            reason: SkipReason::NotALaneState,
            detail: why.clone(),
        });
    }
    let mut states: Vec<&(String, LaneState)> = i.states.iter().collect();
    states.sort_by(|a, b| a.0.cmp(&b.0));

    // Per-file filters.
    let mut kept: Vec<&(String, LaneState)> = Vec::new();
    for entry in states {
        let (stem, st) = entry;
        let name = name_of(&st.role, st);
        let mut skip = |reason, detail: String| {
            out.skipped.push(Skipped {
                role: stem.clone(),
                reason,
                detail,
            });
        };
        if denied(stem) || denied(&st.role) || denied(&name) {
            skip(
                SkipReason::Denied,
                format!("{name:?} is a test fixture name"),
            );
        } else if i.now - st.updated_at > i.since {
            skip(
                SkipReason::TooOld,
                format!(
                    "last written {}, older than {}h",
                    st.updated_at.format("%Y-%m-%d %H:%MZ"),
                    i.since.num_hours()
                ),
            );
        } else if !path_is_under(&st.cwd, &i.cfg.portfolio_root) {
            skip(
                SkipReason::CwdOutsideRoot,
                format!("{} is not under {}", st.cwd, i.cfg.portfolio_root),
            );
        } else if !(i.cwd_exists)(&st.cwd) {
            skip(SkipReason::CwdMissing, format!("{} does not exist", st.cwd));
        } else if st.launch_args.as_ref().is_none_or(|a| a.is_empty()) {
            skip(
                SkipReason::NoLaunchArgs,
                "no recorded launch arguments".into(),
            );
        } else {
            kept.push(entry);
        }
    }

    // One per (name, cwd): the newest file wins; the stem breaks a tie.
    let same = |a: &(String, LaneState), b: &(String, LaneState)| {
        name_of(&a.1.role, &a.1).eq_ignore_ascii_case(&name_of(&b.1.role, &b.1))
            && paths_equal(&a.1.cwd, &b.1.cwd)
    };
    let newest = |e: &&(String, LaneState)| (e.1.updated_at, e.0.clone());
    let mut winners: Vec<&(String, LaneState)> = Vec::new();
    for e in &kept {
        let best = kept
            .iter()
            .filter(|o| same(o, e))
            .max_by_key(|o| newest(o))
            .expect("the entry itself matches");
        if best.0 == e.0 {
            winners.push(e);
        } else {
            out.skipped.push(Skipped {
                role: e.0.clone(),
                reason: SkipReason::Superseded,
                detail: format!("{} is newer for the same name and directory", best.0),
            });
        }
    }

    let mut matched_park: Vec<bool> = vec![false; i.park.len()];
    for (stem, st) in winners {
        let name = name_of(&st.role, st);
        if let Some(r) = i.existing.iter().find(|r| {
            r.name
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(&name))
                && paths_equal(&r.launch_cwd, &st.cwd)
        }) {
            out.skipped.push(Skipped {
                role: stem.clone(),
                reason: SkipReason::AlreadyRegistered,
                detail: format!(
                    "registry record {} has this name in this directory",
                    r.agent_id
                ),
            });
            continue;
        }
        let mut rec = AgentRecord::new(AgentId::generate(), st.cwd.clone(), st.updated_at);
        rec.name = Some(name.clone());
        rec.role = Some(st.role.clone());
        rec.launch_args = st.launch_args.clone();
        rec.model = st.model.clone();
        // Carried as recorded, never raised and never lowered here; `plan` decides what a restore
        // may do with it (bypass is refused there for a non-PM).
        rec.permission_mode = st.permission_mode.clone();
        rec.remote_control = st.remote_control;
        rec.origin = Origin::Imported;
        // The session the file describes: after a reboot its process is gone (`Stopped`, restored
        // as a kill); while it still runs, the process table says so and a restore skips it.
        if st.pid != 0 {
            rec.sessions.push(Session {
                session_id: st.session_id.clone(),
                pid: st.pid,
                process_start_secs: st.pid_start_secs,
                started_at: st.updated_at,
                ended_at: None,
                end_reason: None,
            });
        }
        let parked = i.park.iter().enumerate().any(|(n, p)| {
            let hit = p.eq_ignore_ascii_case(&name)
                || p.eq_ignore_ascii_case(&st.role)
                || p.eq_ignore_ascii_case(stem);
            if hit {
                matched_park[n] = true;
            }
            hit
        });
        if parked {
            rec.intent = Intent::Closed {
                how: ClosedHow::Parked,
                by: IMPORT_ACTOR.to_string(),
                at: i.now,
            };
        }
        out.imports.push(Imported {
            role: stem.clone(),
            record: rec,
        });
    }
    out.unmatched_park = i
        .park
        .iter()
        .zip(matched_park)
        .filter(|(_, m)| !m)
        .map(|(p, _)| p.clone())
        .collect();
    out.imports.sort_by(|a, b| a.role.cmp(&b.role));
    out.skipped.sort_by(|a, b| a.role.cmp(&b.role));
    out
}

/// Parsed lane states, and the files that were not one, each with its file stem.
pub type StateFiles = (Vec<(String, LaneState)>, Vec<(String, String)>);

/// Reads every `*.json` in `dir`. A file that is not a lane state is returned in the second list,
/// never dropped silently. Read-only. Sorted by stem.
pub fn read_states(dir: &Path) -> std::io::Result<StateFiles> {
    let mut good = Vec::new();
    let mut bad = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let path = e?.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|x| x.to_str()) else {
            continue;
        };
        let stem = stem.to_string();
        let parsed = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<LaneState>(&t).map_err(|e| e.to_string()));
        match parsed {
            Ok(s) => good.push((stem, s)),
            Err(why) => bad.push((stem, why)),
        }
    }
    good.sort_by(|a, b| a.0.cmp(&b.0));
    bad.sort_by(|a, b| a.0.cmp(&b.0));
    Ok((good, bad))
}

/// Creates each record. Returns how many were written; stops at the first error.
pub fn write(registry: &Registry, outcome: &Outcome) -> Result<usize, RegistryError> {
    let mut n = 0;
    for imp in &outcome.imports {
        registry.create(&imp.record)?;
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests;
