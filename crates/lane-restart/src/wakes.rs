// SPDX-License-Identifier: MIT OR Apache-2.0

//! The wake-at registry (`docs/WAKE-AT.md`): a copy of each lane's `WAIT:` wake times that the launcher
//! can read while the lane is stopped, so a stopped lane is started at its wake time with no mail.
//!
//! Pure apart from `load`/`save`: every function takes `now` as a parameter (a fake clock in tests).
//! The HANDOFF stays the lane's record; a role's entries are REPLACED wholesale on every write, so an
//! entry cannot outlive the line that made it.

use crate::waits::Wait;
use lane_state::timestamp::Timestamp;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The registry's schema; a file with a newer one is refused, never rewritten.
pub const SCHEMA: u32 = 1;
/// A wake is due at its time plus a stable per-role offset below this, so a burst spreads out.
pub const JITTER_MAX_SECS: i64 = 600;
/// At most this many wake-starts per launcher cycle.
pub const WAKES_PER_CYCLE: usize = 2;
/// A wake's synthetic signal id is `<role>:wake-<compact time>`.
pub const SIGNAL_MARK: &str = "wake-";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wake {
    pub role: String,
    pub event: String,
    pub wake_utc: Timestamp,
    pub registered_utc: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    pub schema: u32,
    pub wakes: Vec<Wake>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            wakes: Vec::new(),
        }
    }
}

impl Registry {
    pub fn path(state_dir: &Path) -> PathBuf {
        state_dir.join("wakes.json")
    }

    /// A missing file is an empty registry; a corrupt or newer-schema file is an error, never "empty"
    /// (an unreadable registry must not look like "no wakes").
    pub fn load(state_dir: &Path) -> Result<Self, String> {
        let text = match std::fs::read_to_string(Self::path(state_dir)) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("{}: {e}", Self::path(state_dir).display())),
        };
        let reg: Self = serde_json::from_str(&text)
            .map_err(|e| format!("{}: {e}", Self::path(state_dir).display()))?;
        if reg.schema > SCHEMA {
            return Err(format!(
                "{}: schema {} is newer than this build's {SCHEMA}",
                Self::path(state_dir).display(),
                reg.schema
            ));
        }
        Ok(reg)
    }

    /// Temp file then rename, so a reader never sees half a registry.
    pub fn save(&self, state_dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(state_dir).map_err(|e| e.to_string())?;
        let path = Self::path(state_dir);
        let tmp = path.with_extension("tmp");
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, json).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Replace every entry of `role` with `waits` (the role's whole truth).
    pub fn replace_role(&mut self, role: &str, waits: &[Wait], now: Timestamp) {
        self.wakes.retain(|w| w.role != role);
        for w in waits {
            let dup = self
                .wakes
                .iter()
                .any(|x| x.role == role && x.event == w.event && x.wake_utc == w.wake);
            if !dup {
                self.wakes.push(Wake {
                    role: role.to_string(),
                    event: w.event.clone(),
                    wake_utc: w.wake,
                    registered_utc: now,
                });
            }
        }
    }

    /// The lane is up: drop its entries whose time has passed.
    pub fn drop_passed(&mut self, role: &str, now: Timestamp) {
        self.wakes
            .retain(|w| !(w.role == role && w.wake_utc <= now));
    }

    /// Remove one entry once the launcher has dealt with it.
    pub fn consume(&mut self, role: &str, event: &str, wake_utc: Timestamp) {
        self.wakes
            .retain(|w| !(w.role == role && w.event == event && w.wake_utc == wake_utc));
    }
}

/// Record `role`'s valid `WAIT:` lines from its HANDOFF `text` (replacing its earlier entries) and
/// return how many are now registered. The caller has already refused a malformed wait.
pub fn register_from_handoff(
    state_dir: &Path,
    role: &str,
    text: &str,
    now: Timestamp,
) -> Result<usize, String> {
    let waits: Vec<Wait> = crate::waits::parse(text)
        .into_iter()
        .filter_map(Result::ok)
        .collect();
    let mut reg = Registry::load(state_dir)?;
    reg.replace_role(role, &waits, now);
    reg.save(state_dir)?;
    Ok(reg.wakes.iter().filter(|w| w.role == role).count())
}

/// Remove the entry the launcher knows by `(role, event, signal_id)`; returns whether one was removed.
pub fn consume_by_signal(
    state_dir: &Path,
    role: &str,
    event: &str,
    signal: &str,
) -> Result<bool, String> {
    let mut reg = Registry::load(state_dir)?;
    let before = reg.wakes.len();
    reg.wakes
        .retain(|w| !(w.role == role && w.event == event && signal_id(w) == signal));
    let removed = reg.wakes.len() != before;
    if removed {
        reg.save(state_dir)?;
    }
    Ok(removed)
}

/// A stable offset in `0..JITTER_MAX_SECS` from the role name (FNV-1a).
pub fn jitter_secs(role: &str) -> i64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in role.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    (h % JITTER_MAX_SECS as u64) as i64
}

/// When the launcher acts on `w`: its time plus the role's jitter.
pub fn due_at(w: &Wake) -> Timestamp {
    w.wake_utc.plus_secs(jitter_secs(&w.role))
}

/// The wakes due at `now`, earliest due time first (ties by role, then event).
pub fn due(reg: &Registry, now: Timestamp) -> Vec<&Wake> {
    let mut v: Vec<&Wake> = reg.wakes.iter().filter(|w| due_at(w) <= now).collect();
    v.sort_by(|a, b| (due_at(a), &a.role, &a.event).cmp(&(due_at(b), &b.role, &b.event)));
    v
}

/// `<role>:wake-<YYYYMMDDTHHMMSSZ>`.
pub fn signal_id(w: &Wake) -> String {
    format!(
        "{}:{SIGNAL_MARK}{}",
        w.role,
        w.wake_utc.format("%Y%m%dT%H%M%SZ")
    )
}

/// Is `signal_id` a wake's synthetic id for `role`?
pub fn is_wake_signal(signal_id: &str, role: &str) -> bool {
    signal_id
        .strip_prefix(role)
        .and_then(|r| r.strip_prefix(':'))
        .is_some_and(|rest| rest.starts_with(SIGNAL_MARK))
}

/// One row of the `wakes` read-out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub role: String,
    pub event: String,
    pub wake_utc: Timestamp,
    pub due_utc: Timestamp,
    pub state: &'static str,
}

/// Every entry with its state: `role-running`, `due-waiting-for-launcher` or `pending`; soonest first.
pub fn readout(reg: &Registry, now: Timestamp, running: &dyn Fn(&str) -> bool) -> Vec<Row> {
    let mut rows: Vec<Row> = reg
        .wakes
        .iter()
        .map(|w| Row {
            role: w.role.clone(),
            event: w.event.clone(),
            wake_utc: w.wake_utc,
            due_utc: due_at(w),
            state: if running(&w.role) {
                "role-running"
            } else if due_at(w) <= now {
                "due-waiting-for-launcher"
            } else {
                "pending"
            },
        })
        .collect();
    rows.sort_by(|a, b| (a.due_utc, &a.role, &a.event).cmp(&(b.due_utc, &b.role, &b.event)));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Timestamp {
        s.parse::<Timestamp>().unwrap()
    }

    fn wait(event: &str, wake: &str) -> Wait {
        Wait {
            line: 1,
            event: event.into(),
            check: "true".into(),
            wake: t(wake),
        }
    }

    const NOW: &str = "2026-11-24T00:00:00Z";

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wakes-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn registry_round_trips_and_a_missing_file_is_empty() {
        let d = tmp("rt");
        assert_eq!(Registry::load(&d).unwrap(), Registry::default());
        let mut r = Registry::default();
        r.replace_role("a", &[wait("e", "2026-11-24T01:00:00Z")], t(NOW));
        r.save(&d).unwrap();
        assert_eq!(Registry::load(&d).unwrap(), r);
        assert!(!d.join("wakes.tmp").exists(), "temp file renamed away");
    }

    #[test]
    fn a_corrupt_or_newer_file_is_refused_not_empty() {
        let d = tmp("bad");
        std::fs::write(Registry::path(&d), "{not json").unwrap();
        assert!(Registry::load(&d).is_err());
        std::fs::write(Registry::path(&d), r#"{"schema":99,"wakes":[]}"#).unwrap();
        assert!(Registry::load(&d).unwrap_err().contains("schema"));
    }

    #[test]
    fn replace_role_drops_a_removed_line_and_keeps_other_roles() {
        let mut r = Registry::default();
        r.replace_role(
            "a",
            &[
                wait("x", "2026-11-24T01:00:00Z"),
                wait("y", "2026-11-24T02:00:00Z"),
            ],
            t(NOW),
        );
        r.replace_role("b", &[wait("z", "2026-11-24T03:00:00Z")], t(NOW));
        r.replace_role("a", &[wait("y", "2026-11-24T02:00:00Z")], t(NOW));
        let mut have: Vec<_> = r
            .wakes
            .iter()
            .map(|w| (w.role.as_str(), w.event.as_str()))
            .collect();
        have.sort();
        assert_eq!(have, vec![("a", "y"), ("b", "z")]);
        r.replace_role("a", &[], t(NOW));
        assert_eq!(
            r.wakes.len(),
            1,
            "no wait lines: the role's entries are gone"
        );
    }

    #[test]
    fn register_from_handoff_replaces_the_roles_entries_and_ignores_prose() {
        let d = tmp("reg");
        let text = "WAIT: a | check: true | wake: 2026-11-24T01:00:00Z
prose WAIT: not one
";
        assert_eq!(register_from_handoff(&d, "r", text, t(NOW)).unwrap(), 1);
        assert_eq!(register_from_handoff(&d, "other", text, t(NOW)).unwrap(), 1);
        // The lane deleted its line: re-registering empties only its own entries.
        assert_eq!(
            register_from_handoff(
                &d,
                "r",
                "no waits
",
                t(NOW)
            )
            .unwrap(),
            0
        );
        let reg = Registry::load(&d).unwrap();
        assert_eq!(reg.wakes.len(), 1);
        assert_eq!(reg.wakes[0].role, "other");
    }

    #[test]
    fn consume_by_signal_removes_only_the_named_entry() {
        let d = tmp("cons");
        let text = "WAIT: a | check: true | wake: 2026-11-24T01:00:00Z
WAIT: b | check: true | wake: 2026-11-24T02:00:00Z
";
        register_from_handoff(&d, "r", text, t(NOW)).unwrap();
        assert!(consume_by_signal(&d, "r", "a", "r:wake-20261124T010000Z").unwrap());
        assert!(
            !consume_by_signal(&d, "r", "a", "r:wake-20261124T010000Z").unwrap(),
            "already gone"
        );
        assert!(
            !consume_by_signal(&d, "r", "b", "r:wake-20261124T010000Z").unwrap(),
            "wrong time"
        );
        assert_eq!(Registry::load(&d).unwrap().wakes.len(), 1);
    }

    #[test]
    fn jitter_is_stable_bounded_and_differs_between_roles() {
        let j = jitter_secs("agentlife");
        assert_eq!(j, jitter_secs("agentlife"));
        assert!((0..JITTER_MAX_SECS).contains(&j));
        let roles = ["a", "b", "c", "d", "e", "f", "g", "h"];
        let set: std::collections::BTreeSet<_> = roles.iter().map(|r| jitter_secs(r)).collect();
        assert!(set.len() > 4, "jitter must spread roles: {set:?}");
    }

    #[test]
    fn a_wake_is_due_only_after_its_time_plus_jitter() {
        let mut r = Registry::default();
        r.replace_role("a", &[wait("e", "2026-11-24T01:00:00Z")], t(NOW));
        let due_at_ = due_at(&r.wakes[0]);
        assert_eq!(
            due_at_,
            t("2026-11-24T01:00:00Z").plus_secs(jitter_secs("a"))
        );
        assert!(due(&r, due_at_.plus_secs(-1)).is_empty());
        assert_eq!(due(&r, due_at_).len(), 1);
    }

    #[test]
    fn a_time_already_past_at_registration_is_due_now() {
        let mut r = Registry::default();
        r.replace_role("a", &[wait("e", "2026-11-23T12:00:00Z")], t(NOW));
        assert_eq!(due(&r, t(NOW)).len(), 1);
    }

    #[test]
    fn due_is_ordered_earliest_first() {
        let mut r = Registry::default();
        r.replace_role("a", &[wait("e", "2026-11-23T05:00:00Z")], t(NOW));
        r.replace_role("b", &[wait("e", "2026-11-23T04:00:00Z")], t(NOW));
        let roles: Vec<_> = due(&r, t(NOW)).iter().map(|w| w.role.clone()).collect();
        assert_eq!(roles, vec!["b", "a"]);
    }

    #[test]
    fn drop_passed_keeps_future_entries_and_consume_removes_one() {
        let mut r = Registry::default();
        r.replace_role(
            "a",
            &[
                wait("old", "2026-11-23T05:00:00Z"),
                wait("new", "2026-11-25T05:00:00Z"),
            ],
            t(NOW),
        );
        r.drop_passed("a", t(NOW));
        assert_eq!(r.wakes.len(), 1);
        assert_eq!(r.wakes[0].event, "new");
        r.consume("a", "new", t("2026-11-25T05:00:00Z"));
        assert!(r.wakes.is_empty());
    }

    #[test]
    fn signal_id_names_the_role_and_is_recognised() {
        let mut r = Registry::default();
        r.replace_role("agentlife", &[wait("e", "2026-11-24T01:02:03Z")], t(NOW));
        let id = signal_id(&r.wakes[0]);
        assert_eq!(id, "agentlife:wake-20261124T010203Z");
        assert!(is_wake_signal(&id, "agentlife"));
        assert!(!is_wake_signal(&id, "other"));
        assert!(!is_wake_signal(
            "agentlife:2026-11-19T15:48:49.123Z",
            "agentlife"
        ));
        assert!(crate::start::signal_names_role(&id, "agentlife"));
    }

    #[test]
    fn readout_states_and_order() {
        let mut r = Registry::default();
        r.replace_role("run", &[wait("e", "2026-11-23T01:00:00Z")], t(NOW));
        r.replace_role("late", &[wait("e", "2026-11-23T02:00:00Z")], t(NOW));
        r.replace_role("soon", &[wait("e", "2026-11-25T02:00:00Z")], t(NOW));
        let rows = readout(&r, t(NOW), &|role| role == "run");
        let st: Vec<_> = rows.iter().map(|x| (x.role.as_str(), x.state)).collect();
        assert_eq!(
            st,
            vec![
                ("run", "role-running"),
                ("late", "due-waiting-for-launcher"),
                ("soon", "pending")
            ]
        );
    }
}
