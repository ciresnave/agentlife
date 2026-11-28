// SPDX-License-Identifier: MIT OR Apache-2.0

//! What only a launcher caller's `lane-restart start` is held to, after the spec checks of
//! `plan_start` and before anything spawns (`docs/LAUNCHER-SERVICE.md`, "Caps, ledger and floor";
//! OverMind `docs/LANE-LAUNCH-POLICY.md` rules 5-6 and section 3):
//!
//! * **`restore.lock`**: the same file `agentlife restore` takes (`<agentlife home>/restore.lock`,
//!   body `<pid> <start secs>`), so a launcher start and a restore never spawn at once. Held from the
//!   caps check to the end of the start, which also makes the caps check race-free.
//! * **Caps**, counted from `restart.log`: spawns by launcher callers in the last hour (per role
//!   and overall) and in the last day (per role). Only a row where a spawn was *attempted* counts:
//!   refusals, `already-running`, `held-for-memory` and `lock-held` never consume budget.
//! * **The memory floor** `agentlife restore` holds (`free_ram_floor_gb`, same file and variable).
//!
//! Everything here is a function of injected inputs (rows, a clock value, free memory, a process
//! table), so each guard is testable without a window.

use crate::facts::SystemFacts;
use crate::ledger::Row;
use crate::start::{is_launcher_caller, StartRefusal};
use chrono::{DateTime, Duration, Utc};
use std::path::{Path, PathBuf};

/// OverMind policy section 2 rule 6 (proposals, not measurements): spawns per role per hour.
pub const MAX_SPAWNS_PER_ROLE_PER_HOUR: usize = 3;
/// OverMind policy section 2 rule 6: spawns across all roles per hour.
pub const MAX_SPAWNS_PER_HOUR: usize = 24;
/// A per-role daily bound. The policy names none; this number is the lane's proposal, awaiting the
/// PM's (3 an hour is the burst allowance, this is the "keeps coming back all day" alarm).
pub const MAX_SPAWNS_PER_ROLE_PER_DAY: usize = 8;
/// PM ruling 2026-11-26 (jds-kt): a start left at the dialog spends the DAILY cap only while it is younger than this
/// or a `settle` row says its session ran (`docs/LAUNCHER-CAP-DESIGN.md` A1). The hourly caps always count it.
pub const SETTLE_SECS: i64 = 10 * 60;
/// The ledger `kind` of a row saying whether a started session ran (`ran`), keyed by `signal_id`.
pub const SETTLE_KIND: &str = "settle";

/// Appends the `settle` row for a start left at the dialog whose session the launcher then saw online
/// (`ran: true`, keyed by `signal_id`; `docs/LAUNCHER-CAP-DESIGN.md` A1) to `<state_dir>/restart.log`.
/// `true` when the line was written.
pub fn write_settle_row(state_dir: &Path, role: &str, signal_id: &str) -> bool {
    let entry = crate::log::Entry {
        timestamp: lane_state::timestamp::Timestamp::now(),
        requested_by: crate::start::LAUNCHER_ROLE,
        role,
        outcome: "settled: the session was seen online".to_string(),
        acted: false,
        kind: Some(SETTLE_KIND),
        caller_role: None,
        model: None,
        reason: None,
        signal_id: Some(signal_id),
        result: Some("settled:ran"),
        spawned: None,
        ran: Some(true),
    };
    crate::log::append(&state_dir.join("restart.log"), &entry).is_ok()
}

/// What a start's `<outcome>:<reason>` result says about whether its session ran: `Some(true)` reached
/// progress, `Some(false)` the spawn failed, `None` unknown (left at the dialog) or not a spawn.
pub fn ran_from_result(result: &str) -> Option<bool> {
    match result {
        "started:started" => Some(true),
        "failed:spawn-failed" => Some(false),
        _ => None,
    }
}

/// `free_ram_floor_gb`'s default, equal to `agentlife`'s own (`Config::default`).
pub const DEFAULT_FREE_RAM_FLOOR_GB: f64 = 8.0;

/// The ledger `kind` of a start (`log_ledger(.., "start", ..)`).
const START_KIND: &str = "start";

/// Which cap a launcher start ran into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapHit {
    RolePerHour,
    OverallPerHour,
    RolePerDay,
}

impl CapHit {
    /// The vocabulary-A reason code: the two hourly caps are the policy's one `rate`.
    pub fn reason(self) -> &'static str {
        match self {
            CapHit::RolePerHour | CapHit::OverallPerHour => "rate",
            CapHit::RolePerDay => "rate-daily",
        }
    }
}

impl std::fmt::Display for CapHit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CapHit::RolePerHour => write!(
                f,
                "{MAX_SPAWNS_PER_ROLE_PER_HOUR} launcher spawns of this role in the last hour"
            ),
            CapHit::OverallPerHour => write!(
                f,
                "{MAX_SPAWNS_PER_HOUR} launcher spawns across all roles in the last hour"
            ),
            CapHit::RolePerDay => write!(
                f,
                "{MAX_SPAWNS_PER_ROLE_PER_DAY} launcher spawns of this role in the last day"
            ),
        }
    }
}

/// Launcher spawns in the windows the caps read.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SpawnCounts {
    pub role_hour: usize,
    pub all_hour: usize,
    pub role_day: usize,
}

/// A row is a launcher spawn when a launcher caller's `start` actually tried to spawn. Anything
/// else (a refusal, a dry run, a manual PM start) is not budget.
fn is_launcher_spawn(r: &Row) -> bool {
    r.kind.as_deref() == Some(START_KIND)
        && r.spawned == Some(true)
        && r.caller_role.as_deref().is_some_and(is_launcher_caller)
}

/// Whether a spawn row spends the daily cap: not when its session did not run. A row from before
/// `ran` (no `ran`, a result other than a dialog wait) counts as it always did.
fn spends_daily(
    r: &Row,
    now: DateTime<Utc>,
    settled_ran: &std::collections::HashSet<&str>,
) -> bool {
    match r.ran {
        Some(ran) => ran,
        None if r.result.as_deref() == Some("started:awaiting-dialog") => {
            now - r.timestamp < Duration::seconds(SETTLE_SECS)
                || r.signal_id
                    .as_deref()
                    .is_some_and(|s| settled_ran.contains(s))
        }
        None => true,
    }
}

/// Counts launcher spawns in the last hour and day ending at `now`. A row stamped in the future
/// counts (a skewed clock must not hide spawns).
pub fn count_spawns(rows: &[Row], role: &str, now: DateTime<Utc>) -> SpawnCounts {
    let hour = now - Duration::hours(1);
    let day = now - Duration::hours(24);
    let mut c = SpawnCounts::default();
    let settled_ran: std::collections::HashSet<&str> = rows
        .iter()
        .filter(|r| r.kind.as_deref() == Some(SETTLE_KIND) && r.ran == Some(true))
        .filter_map(|r| r.signal_id.as_deref())
        .collect();
    for r in rows.iter().filter(|r| is_launcher_spawn(r)) {
        let mine = r.role == role;
        if r.timestamp > hour {
            c.all_hour += 1;
            if mine {
                c.role_hour += 1;
            }
        }
        if mine && r.timestamp > day && spends_daily(r, now, &settled_ran) {
            c.role_day += 1;
        }
    }
    c
}

/// The first cap `counts` has reached (a start would be the N+1th), if any.
pub fn check_caps(counts: &SpawnCounts) -> Result<(), CapHit> {
    if counts.role_hour >= MAX_SPAWNS_PER_ROLE_PER_HOUR {
        return Err(CapHit::RolePerHour);
    }
    if counts.all_hour >= MAX_SPAWNS_PER_HOUR {
        return Err(CapHit::OverallPerHour);
    }
    if counts.role_day >= MAX_SPAWNS_PER_ROLE_PER_DAY {
        return Err(CapHit::RolePerDay);
    }
    Ok(())
}

/// The memory floor, resolved the way `agentlife` does for the one key: the environment variable
/// `AGENTLIFE_FREE_RAM_FLOOR_GB` over `free_ram_floor_gb` in `<home>/config.json` over the default.
/// Anything unreadable or out of range is an `Err` (fail closed): a typo must never read as "no
/// floor". The other keys of the file are not read here (agentlife rejects an unknown key; this
/// reader does not need to).
pub fn resolve_floor_gb(config_file: &Path, env_value: Option<&str>) -> Result<f64, String> {
    let from_env = match env_value {
        Some(v) => Some(
            v.trim()
                .parse::<f64>()
                .map_err(|e| format!("AGENTLIFE_FREE_RAM_FLOOR_GB {v:?}: {e}"))?,
        ),
        None => None,
    };
    let floor = match from_env {
        Some(v) => v,
        None => match std::fs::read_to_string(config_file) {
            Ok(text) => {
                let v: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| format!("{}: {e}", config_file.display()))?;
                match v.get("free_ram_floor_gb") {
                    None => DEFAULT_FREE_RAM_FLOOR_GB,
                    Some(n) => n.as_f64().ok_or_else(|| {
                        format!(
                            "{}: free_ram_floor_gb is not a number",
                            config_file.display()
                        )
                    })?,
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => DEFAULT_FREE_RAM_FLOOR_GB,
            Err(e) => return Err(format!("{}: {e}", config_file.display())),
        },
    };
    if !floor.is_finite() || floor < 0.0 {
        return Err(format!(
            "free_ram_floor_gb must be a finite number >= 0, got {floor}"
        ));
    }
    Ok(floor)
}

/// `agentlife`'s home: `AGENTLIFE_HOME` when set and non-empty, else the platform default.
pub fn agentlife_home(env_value: Option<&str>) -> PathBuf {
    match env_value {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => lane_state::platform::agentlife_home(),
    }
}

/// Free memory below the floor. An unmeasurable memory (`None`) passes, exactly as it does in
/// `agentlife restore`: the floor guards a measured shortage, and the two paths must not disagree.
pub fn memory_held(free_gb: Option<f64>, floor_gb: f64) -> Option<f64> {
    free_gb.filter(|g| *g < floor_gb)
}

/// Available memory in GiB, as `agentlife restore` measures it; `None` when the OS reports none.
pub fn free_memory_gb() -> Option<f64> {
    let Ok(mut sys) = sysinfo::System::new() else {
        return None;
    };
    sys.refresh_memory();
    let bytes = sys.available_memory();
    (bytes > 0).then(|| bytes as f64 / (1u64 << 30) as f64)
}

/// Why `restore.lock` was not taken.
#[derive(Debug, PartialEq, Eq)]
pub enum RestoreLockError {
    /// A live `agentlife restore` (or another launcher start) holds it.
    Held {
        pid: u32,
    },
    Unavailable(String),
}

/// `<home>/restore.lock`, held while a launcher start runs. Same file and same body
/// (`<pid> <start secs>`) as `agentlife`'s `RestoreLock`, so each sees the other's.
pub struct RestoreLockGuard {
    path: PathBuf,
    token: String,
}

impl std::fmt::Debug for RestoreLockGuard {
    /// The token is the lock's secret; it is never printed (portfolio rule SEC-012).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestoreLockGuard")
            .field("path", &self.path)
            .field("token", &format_args!("<redacted>"))
            .finish()
    }
}

impl RestoreLockGuard {
    pub fn acquire(home: &Path, facts: &dyn SystemFacts) -> Result<Self, RestoreLockError> {
        let me = std::process::id();
        let my_start = facts
            .process_identity(me)
            .map(|i| i.start_time_secs)
            .ok_or_else(|| {
                RestoreLockError::Unavailable("this process's start time is unreadable".into())
            })?;
        let path = home.join("restore.lock");
        std::fs::create_dir_all(home).map_err(|e| RestoreLockError::Unavailable(e.to_string()))?;
        let token = format!("{me} {my_start}");
        for _ in 0..3 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    use std::io::Write;
                    f.write_all(token.as_bytes())
                        .map_err(|e| RestoreLockError::Unavailable(e.to_string()))?;
                    return Ok(Self { path, token });
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::PermissionDenied
                    ) =>
                {
                    let text = std::fs::read_to_string(&path).unwrap_or_default();
                    let owner = parse_owner(&text);
                    if let Some((pid, start)) = owner {
                        let alive = facts
                            .process_identity(pid)
                            .is_some_and(|i| i.start_time_secs == start);
                        if alive {
                            return Err(RestoreLockError::Held { pid });
                        }
                    }
                    // Its owner is gone (or the file is unreadable): reclaim it. Rename first, so
                    // that of several reclaimers only one wins.
                    let grave = path.with_extension(format!("stale.{me}"));
                    if std::fs::rename(&path, &grave).is_ok() {
                        let _ = std::fs::remove_file(&grave);
                    }
                }
                Err(e) => return Err(RestoreLockError::Unavailable(e.to_string())),
            }
        }
        Err(RestoreLockError::Unavailable(
            "could not reclaim a stale lock".into(),
        ))
    }
}

fn parse_owner(text: &str) -> Option<(u32, u64)> {
    let mut it = text.split_whitespace();
    let pid = it.next()?.parse().ok()?;
    let start = it.next()?.parse().ok()?;
    Some((pid, start))
}

impl Drop for RestoreLockGuard {
    fn drop(&mut self) {
        // Only this run's own file: never delete a lock someone else reclaimed and retook.
        if std::fs::read_to_string(&self.path).is_ok_and(|t| t == self.token) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Reads the ledger for the caps. A file that is missing is an empty ledger (nothing was ever
/// spawned); one that cannot be read, or has a line that is not an entry, is an `Err`: a spawn row
/// might be hiding in it, and the caps must fail closed rather than reset.
pub fn load_rows(path: &Path) -> Result<Vec<Row>, String> {
    let read = crate::ledger::read(path)?;
    if let Some((line, why)) = read.unreadable.first() {
        return Err(format!(
            "{}: {} line(s) are not entries (first: line {line}: {why})",
            path.display(),
            read.unreadable.len()
        ));
    }
    Ok(read.rows)
}

/// `restore.lock` as a start refusal: held is `failed: lock-held`, unreadable is
/// `failed: restore-lock-unavailable`.
pub fn take_restore_lock(
    home: &Path,
    facts: &dyn SystemFacts,
) -> Result<RestoreLockGuard, StartRefusal> {
    RestoreLockGuard::acquire(home, facts).map_err(|e| match e {
        RestoreLockError::Held { pid } => StartRefusal::RestoreLockHeld { pid },
        RestoreLockError::Unavailable(why) => StartRefusal::GateUnavailable {
            reason: "restore-lock-unavailable",
            why,
        },
    })
}

/// The caps, then the memory floor, for one launcher start (called with `restore.lock` held, so
/// two starts cannot both pass the caps). First refusal wins.
pub fn decide(
    rows: Result<Vec<Row>, String>,
    role: &str,
    now: DateTime<Utc>,
    floor_gb: Result<f64, String>,
    free_gb: Option<f64>,
) -> Result<(), StartRefusal> {
    let rows = rows.map_err(|why| StartRefusal::GateUnavailable {
        reason: "store-unavailable",
        why,
    })?;
    check_caps(&count_spawns(&rows, role, now)).map_err(StartRefusal::RateCap)?;
    let floor_gb = floor_gb.map_err(|why| StartRefusal::GateUnavailable {
        reason: "config-unreadable",
        why,
    })?;
    if let Some(free_gb) = memory_held(free_gb, floor_gb) {
        return Err(StartRefusal::HeldForMemory { free_gb, floor_gb });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{log, RecordingFacts};
    use chrono::TimeZone;
    use tempfile::tempdir;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 16, 12, 0, 0).unwrap()
    }

    fn row(role: &str, mins_ago: i64, caller: Option<&str>, spawned: Option<bool>) -> Row {
        Row {
            timestamp: now() - Duration::minutes(mins_ago),
            requested_by: caller.unwrap_or("pm").to_string(),
            role: role.to_string(),
            outcome: "x".to_string(),
            acted: spawned == Some(true),
            kind: Some("start".to_string()),
            caller_role: caller.map(String::from),
            model: None,
            reason: None,
            signal_id: None,
            result: None,
            spawned,
            ran: None,
        }
    }

    fn start_row(role: &str, mins_ago: i64, signal: &str, result: &str, ran: Option<bool>) -> Row {
        let mut r = row(role, mins_ago, Some(L), Some(true));
        r.signal_id = Some(signal.to_string());
        r.result = Some(result.to_string());
        r.ran = ran;
        r
    }

    const L: &str = "launcher:agentlife-launcher";

    #[test]
    fn only_a_launcher_spawn_inside_the_window_counts() {
        let rows = vec![
            row("a", 10, Some(L), Some(true)), // counts: role, hour, day
            row("a", 30 * 60 / 10, Some(L), Some(true)), // 3 h ago: day only
            row("b", 5, Some(L), Some(true)),  // other role: overall only
            row("a", 5, Some(L), Some(false)), // refusal: nothing
            row("a", 5, Some(L), None),        // a row from before 0.15.13: nothing
            row("a", 5, Some("launcher:someone-else"), Some(true)), // not a launcher record
            row("a", 5, Some("pm"), Some(true)), // the PM's own start: not launcher budget
            row("a", 25 * 60, Some(L), Some(true)), // 25 h ago: outside the day
        ];
        let c = count_spawns(&rows, "a", now());
        assert_eq!(
            c,
            SpawnCounts {
                role_hour: 1,
                all_hour: 2,
                role_day: 2
            }
        );
    }

    // docs/LAUNCHER-CAP-DESIGN.md tests 1-3 and 6 (PM ruling jds-kt: 8 a day, 10 min settle).
    #[test]
    fn a_dialog_stuck_start_older_than_the_settle_window_is_hourly_but_not_daily() {
        let r = start_row("a", 11, "s1", "started:awaiting-dialog", None);
        let c = count_spawns(&[r], "a", now());
        assert_eq!((c.role_hour, c.all_hour, c.role_day), (1, 1, 0));
    }

    #[test]
    fn a_dialog_start_inside_the_settle_window_still_counts_daily() {
        let r = start_row("a", 9, "s1", "started:awaiting-dialog", None);
        assert_eq!(count_spawns(&[r], "a", now()).role_day, 1);
    }

    #[test]
    fn a_dialog_start_that_a_settle_row_says_ran_counts_daily() {
        let r = start_row("a", 30, "s1", "started:awaiting-dialog", None);
        let mut s = row("a", 20, Some(L), None);
        s.kind = Some("settle".into());
        s.signal_id = Some("s1".into());
        s.ran = Some(true);
        assert_eq!(count_spawns(&[r, s], "a", now()).role_day, 1);
    }

    #[test]
    fn a_failed_spawn_is_hourly_but_not_daily() {
        let r = start_row("a", 5, "s1", "failed:spawn-failed", Some(false));
        let c = count_spawns(&[r], "a", now());
        assert_eq!((c.role_hour, c.role_day), (1, 0));
    }

    #[test]
    fn eight_sessions_that_ran_refuse_the_ninth_but_eight_stuck_ones_do_not() {
        let ran: Vec<Row> = (0..8)
            .map(|i| {
                start_row(
                    "a",
                    100 + i,
                    &format!("r{i}"),
                    "started:started",
                    Some(true),
                )
            })
            .collect();
        let c = count_spawns(&ran, "a", now());
        assert_eq!(check_caps(&c), Err(CapHit::RolePerDay));
        let stuck: Vec<Row> = (0..8)
            .map(|i| {
                start_row(
                    "a",
                    100 + i,
                    &format!("d{i}"),
                    "started:awaiting-dialog",
                    None,
                )
            })
            .collect();
        assert_eq!(check_caps(&count_spawns(&stuck, "a", now())), Ok(()));
    }

    #[test]
    fn a_row_from_before_the_ran_field_counts_as_it_always_did() {
        let mut r = row("a", 100, Some(L), Some(true));
        r.result = Some("started:started".into());
        assert_eq!(count_spawns(&[r], "a", now()).role_day, 1);
        let bare = row("a", 100, Some(L), Some(true));
        assert_eq!(count_spawns(&[bare], "a", now()).role_day, 1);
    }

    /// The writer and the counter agree: a dialog start older than the window is 0 daily until the settle row
    /// the launcher writes lands, then 1. Goes through the real file.
    #[test]
    fn the_settle_row_the_launcher_writes_makes_the_dialog_start_count_daily() {
        let d = tempdir().unwrap();
        let start = start_row("a", 30, "s1", "started:awaiting-dialog", None);
        assert_eq!(
            count_spawns(std::slice::from_ref(&start), "a", now()).role_day,
            0
        );
        assert!(write_settle_row(d.path(), "a", "s1"));
        let read = crate::ledger::read(&d.path().join("restart.log")).unwrap();
        assert!(read.unreadable.is_empty());
        let mut rows = vec![start];
        rows.extend(read.rows);
        assert_eq!(rows[1].kind.as_deref(), Some(SETTLE_KIND));
        assert_eq!(rows[1].ran, Some(true));
        assert_eq!(count_spawns(&rows, "a", now()).role_day, 1);
    }

    #[test]
    fn ran_is_read_from_the_result_a_start_writes() {
        assert_eq!(ran_from_result("started:started"), Some(true));
        assert_eq!(ran_from_result("started:awaiting-dialog"), None);
        assert_eq!(ran_from_result("failed:spawn-failed"), Some(false));
        assert_eq!(ran_from_result("refused:rate"), None);
    }

    #[test]
    fn a_kind_other_than_start_is_not_a_spawn() {
        let mut r = row("a", 5, Some(L), Some(true));
        r.kind = Some("restart".into());
        assert_eq!(count_spawns(&[r], "a", now()), SpawnCounts::default());
    }

    #[test]
    fn a_future_stamped_row_still_counts() {
        let r = row("a", -30, Some(L), Some(true));
        assert_eq!(count_spawns(&[r], "a", now()).role_hour, 1);
    }

    #[test]
    fn the_fourth_spawn_of_a_role_in_an_hour_is_refused_the_third_is_not() {
        let two = SpawnCounts {
            role_hour: 2,
            ..Default::default()
        };
        assert_eq!(check_caps(&two), Ok(()));
        let three = SpawnCounts {
            role_hour: 3,
            ..Default::default()
        };
        assert_eq!(check_caps(&three), Err(CapHit::RolePerHour));
        assert_eq!(CapHit::RolePerHour.reason(), "rate");
    }

    #[test]
    fn the_overall_cap_and_the_ninth_in_a_day_are_refused() {
        assert_eq!(MAX_SPAWNS_PER_HOUR, 24);
        let eleven = SpawnCounts {
            all_hour: MAX_SPAWNS_PER_HOUR - 1,
            ..Default::default()
        };
        assert_eq!(check_caps(&eleven), Ok(()));
        let twelve = SpawnCounts {
            all_hour: MAX_SPAWNS_PER_HOUR,
            ..Default::default()
        };
        assert_eq!(check_caps(&twelve), Err(CapHit::OverallPerHour));
        let day = SpawnCounts {
            role_day: MAX_SPAWNS_PER_ROLE_PER_DAY,
            ..Default::default()
        };
        let hit = check_caps(&day).unwrap_err();
        assert_eq!((hit, hit.reason()), (CapHit::RolePerDay, "rate-daily"));
    }

    #[test]
    fn the_floor_is_env_then_file_then_default_and_a_bad_value_is_an_error() {
        let d = tempdir().unwrap();
        let cfg = d.path().join("config.json");
        assert_eq!(resolve_floor_gb(&cfg, None), Ok(8.0)); // no file: the default
        std::fs::write(&cfg, r#"{"free_ram_floor_gb": 12.5, "max_running": 3}"#).unwrap();
        assert_eq!(resolve_floor_gb(&cfg, None), Ok(12.5));
        assert_eq!(resolve_floor_gb(&cfg, Some(" 16 ")), Ok(16.0)); // env wins
        std::fs::write(&cfg, r#"{"max_running": 3}"#).unwrap();
        assert_eq!(resolve_floor_gb(&cfg, None), Ok(8.0)); // key absent
        for bad in ["NaN", "inf", "-1", "eight", ""] {
            assert!(resolve_floor_gb(&cfg, Some(bad)).is_err(), "{bad:?}");
        }
        for bad in [
            r#"{"free_ram_floor_gb": "8"}"#,
            r#"{"free_ram_floor_gb": -2}"#,
            "{",
        ] {
            std::fs::write(&cfg, bad).unwrap();
            assert!(resolve_floor_gb(&cfg, None).is_err(), "{bad}");
        }
    }

    #[test]
    fn memory_below_the_floor_holds_and_an_unmeasured_memory_does_not() {
        assert_eq!(memory_held(Some(7.9), 8.0), Some(7.9));
        assert_eq!(memory_held(Some(8.0), 8.0), None); // restore: `g < floor`
        assert_eq!(memory_held(None, 8.0), None);
    }

    fn me_facts(start: u64) -> RecordingFacts {
        let l = log();
        let mut f = RecordingFacts::new(&l);
        f.live_pid = Some((std::process::id(), start));
        f
    }

    #[test]
    fn the_lock_is_taken_released_and_the_body_is_pid_and_start() {
        let d = tempdir().unwrap();
        let f = me_facts(4242);
        let g = RestoreLockGuard::acquire(d.path(), &f).unwrap();
        let body = std::fs::read_to_string(d.path().join("restore.lock")).unwrap();
        assert_eq!(body, format!("{} 4242", std::process::id()));
        // Debug must not print the token (no value in the failure message either).
        let shown = format!("{g:?}");
        assert!(
            !shown.contains(&g.token),
            "Debug output of RestoreLockGuard contains the token"
        );
        assert!(
            shown.contains("<redacted>"),
            "the token field must say it is redacted"
        );
        drop(g);
        assert!(!d.path().join("restore.lock").exists());
    }

    #[test]
    fn a_live_owner_holds_it_and_the_file_is_left_alone() {
        let d = tempdir().unwrap();
        // the owner is "this process" at start 4242; the second taker is told so
        std::fs::write(
            d.path().join("restore.lock"),
            format!("{} 4242", std::process::id()),
        )
        .unwrap();
        let f = me_facts(4242);
        let err = RestoreLockGuard::acquire(d.path(), &f).unwrap_err();
        assert_eq!(
            err,
            RestoreLockError::Held {
                pid: std::process::id()
            }
        );
        assert!(d.path().join("restore.lock").exists());
    }

    #[test]
    fn a_recycled_pid_or_a_garbled_body_is_reclaimed() {
        let d = tempdir().unwrap();
        let p = d.path().join("restore.lock");
        // same pid, a different start time: the recorded owner is gone
        std::fs::write(&p, format!("{} 1", std::process::id())).unwrap();
        let f = me_facts(4242);
        let g = RestoreLockGuard::acquire(d.path(), &f).unwrap();
        drop(g);
        std::fs::write(&p, "not an owner").unwrap();
        let g = RestoreLockGuard::acquire(d.path(), &f).unwrap();
        assert!(p.exists());
        drop(g);
        assert!(!p.exists());
    }

    #[test]
    fn dropping_does_not_delete_a_lock_that_someone_else_now_owns() {
        let d = tempdir().unwrap();
        let f = me_facts(4242);
        let g = RestoreLockGuard::acquire(d.path(), &f).unwrap();
        std::fs::write(d.path().join("restore.lock"), "9999 1").unwrap();
        drop(g);
        assert_eq!(
            std::fs::read_to_string(d.path().join("restore.lock")).unwrap(),
            "9999 1"
        );
    }

    #[test]
    fn an_unreadable_own_start_time_is_unavailable_not_held() {
        let d = tempdir().unwrap();
        let l = log();
        let f = RecordingFacts::new(&l); // live_pid None: this process is not in the table
        assert!(matches!(
            RestoreLockGuard::acquire(d.path(), &f),
            Err(RestoreLockError::Unavailable(_))
        ));
    }

    fn ok_rows(n: usize, role: &str) -> Result<Vec<Row>, String> {
        Ok((0..n)
            .map(|i| row(role, 5 + i as i64, Some(L), Some(true)))
            .collect())
    }

    #[test]
    fn decide_answers_in_order_caps_then_config_then_memory() {
        let outcome = |r: Result<(), StartRefusal>| r.map_err(|e| e.outcome());
        // nothing wrong
        assert_eq!(
            outcome(decide(ok_rows(0, "a"), "a", now(), Ok(8.0), Some(20.0))),
            Ok(())
        );
        // the cap is `refused: rate`, even with low memory
        assert_eq!(
            outcome(decide(ok_rows(3, "a"), "a", now(), Ok(8.0), Some(1.0))),
            Err(("refused", "rate"))
        );
        // another role's spawns do not consume this role's per-role budget
        assert_eq!(
            outcome(decide(ok_rows(3, "b"), "a", now(), Ok(8.0), Some(20.0))),
            Ok(())
        );
        assert_eq!(
            outcome(decide(ok_rows(0, "a"), "a", now(), Ok(8.0), Some(7.0))),
            Err(("failed", "held-for-memory"))
        );
        assert_eq!(
            outcome(decide(
                ok_rows(0, "a"),
                "a",
                now(),
                Err("bad".into()),
                Some(20.0)
            )),
            Err(("failed", "config-unreadable"))
        );
        assert_eq!(
            outcome(decide(Err("gone".into()), "a", now(), Ok(8.0), Some(20.0))),
            Err(("failed", "store-unavailable"))
        );
    }

    #[test]
    fn a_missing_ledger_is_empty_and_a_non_entry_line_fails_closed() {
        let d = tempdir().unwrap();
        let p = d.path().join("restart.log");
        assert_eq!(load_rows(&p), Ok(Vec::new()));
        let good = r#"{"timestamp":"2026-10-16T11:55:00Z","requested_by":"launcher:agentlife-launcher","role":"a","outcome":"x","acted":true,"kind":"start","caller_role":"launcher:agentlife-launcher","spawned":true}"#;
        std::fs::write(
            &p,
            format!(
                "{good}
"
            ),
        )
        .unwrap();
        let rows = load_rows(&p).unwrap();
        assert_eq!(count_spawns(&rows, "a", now()).role_hour, 1);
        std::fs::write(
            &p,
            format!(
                "{good}
{{\"trunc
"
            ),
        )
        .unwrap();
        let err = load_rows(&p).unwrap_err();
        assert!(err.contains("line 2"), "{err}");
    }

    #[test]
    fn restore_lock_maps_to_lock_held_and_unavailable() {
        let d = tempdir().unwrap();
        std::fs::write(
            d.path().join("restore.lock"),
            format!("{} 4242", std::process::id()),
        )
        .unwrap();
        let f = me_facts(4242);
        let e = take_restore_lock(d.path(), &f).unwrap_err();
        assert_eq!(e.outcome(), ("failed", "lock-held"));
        let l = log();
        let none = RecordingFacts::new(&l);
        let e = take_restore_lock(d.path(), &none).unwrap_err();
        assert_eq!(e.outcome(), ("failed", "restore-lock-unavailable"));
    }
}
