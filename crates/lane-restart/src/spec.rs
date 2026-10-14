// SPDX-License-Identifier: MIT OR Apache-2.0

//! `C:/Projects/.lane-state/<role>.launch.json` - RESTART-TOOL-DESIGN.md section 14.4.
//!
//! The durable record of HOW a lane is launched, so a lane that ends (stopped on purpose, or
//! crashed) can be started again with the parameters it had. The invariant it serves, section 14:
//! **a lane may be suspended only if it can be started again** - the spec is written, complete and
//! read back, BEFORE the process is touched.
//!
//! Three things this file is deliberately not:
//! * a copy of the argv: it is an allowlist serialisation (`relaunch::extra_launch_args`'s table),
//!   and a value that could carry a secret (an inline JSON `--mcp-config` / `--settings`) is
//!   refused, not stored;
//! * a lane state: it has no top-level `session_id` or `pid`, so the readers of this directory
//!   that try every `*.json` as a `LaneState` (`lane_state_writer::all_states`, agentlife's
//!   `import::read_states`) never take it for one;
//! * authority: nothing here grants a start; `lane-start` (a later change) decides that.

use crate::facts::SystemFacts;
use crate::launch::{prepare_launch, LaunchSpec, PreparedLaunch};
use crate::relaunch::{extra_launch_args, valid_identifier, RelaunchError};
use crate::state;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const SCHEMA: u32 = 1;

/// A file name ending like this is a launch spec, not a lane state.
pub const FILE_SUFFIX: &str = ".launch.json";

/// Leftover temp files older than this are swept by the next write for the same role.
const TEMP_SWEEP_AFTER: std::time::Duration = std::time::Duration::from_secs(60 * 60);

pub fn is_spec_file_name(name: &str) -> bool {
    name.ends_with(FILE_SUFFIX)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecState {
    /// Written at a confirmed launch: the lane was up with these parameters.
    Running,
    /// Written, complete and read back, immediately before the kill.
    Stopping,
    /// The process is confirmed gone and the spec is the way back.
    Suspended,
    /// Imported from a lane state whose process was already gone (`specs import`): the lane
    /// ended without a stop, and this is its last known launch.
    Crashed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecFile {
    pub schema: u32,
    pub role: String,
    pub name: String,
    pub cwd: String,
    /// The explicit `--model` value a start passes.
    pub model: String,
    pub permission_mode: Option<String>,
    pub remote_control: bool,
    /// Only what `relaunch::extra_launch_args` allows; never the raw argv.
    pub launch_args: Vec<String>,
    pub handoff_path: Option<String>,
    pub handoff_mtime: Option<DateTime<Utc>>,
    pub state: SpecState,
    pub written_at: DateTime<Utc>,
    /// Who caused this write: `restart`, `launch`, ...
    pub written_by: String,
    /// The session this spec was derived from when it was stopped. Not named `session_id` or
    /// `pid` on purpose: see the module note on `LaneState`.
    pub stopped_session_id: Option<String>,
    pub stopped_pid_start_secs: Option<u64>,
    pub stop_reason: Option<String>,
    pub tool_version: String,
}

#[derive(Debug, PartialEq)]
pub enum SpecError {
    InvalidRole(String),
    /// `--mcp-config` / `--settings` given as inline JSON: it may carry a secret, so it is never
    /// stored, and a spec without it would not be the launch it describes.
    InlineConfig(String),
    /// The spec as built would not pass `prepare_launch`; nothing was written.
    Incomplete(String),
    NotFound(String),
    Unreadable(String),
    Malformed(String),
    UnsupportedSchema(u32),
    Write(String),
    /// What was read back is not what was written.
    ReadBackMismatch,
}

impl std::fmt::Display for SpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpecError::InvalidRole(r) => write!(f, "not a plain role identifier: {r:?}"),
            SpecError::InlineConfig(flag) => write!(
                f,
                "{flag} was given inline JSON, which may hold a secret and is never stored; \
                 launch it from a file path to make the lane restartable"
            ),
            SpecError::Incomplete(why) => write!(f, "the launch spec is not complete: {why}"),
            SpecError::NotFound(p) => write!(f, "no launch spec at {p}"),
            SpecError::Unreadable(e) => write!(f, "could not read the launch spec: {e}"),
            SpecError::Malformed(e) => write!(f, "the launch spec is not valid: {e}"),
            SpecError::UnsupportedSchema(n) => write!(
                f,
                "the launch spec has schema {n}; this build reads schema {SCHEMA}"
            ),
            SpecError::Write(e) => write!(f, "could not write the launch spec: {e}"),
            SpecError::ReadBackMismatch => {
                write!(f, "the launch spec read back differs from what was written")
            }
        }
    }
}

/// What a spec is derived from.
pub struct SpecInputs<'a> {
    pub launch: &'a LaunchSpec,
    pub model: &'a str,
    pub session_id: Option<&'a str>,
    pub pid_start_secs: Option<u64>,
    pub state: SpecState,
    pub written_by: &'a str,
    pub now: DateTime<Utc>,
}

/// Flags whose value may be inline JSON (and so a secret) rather than a file path.
const INLINE_CAPABLE_FLAGS: &[&str] = &["--mcp-config", "--settings"];

/// The HANDOFF file the lane writes: `<cwd>/HANDOFF.md`, else `<cwd>/<ROLE>-HANDOFF.md`.
pub fn find_handoff(cwd: &str, role: &str) -> Option<(String, DateTime<Utc>)> {
    let upper = role.to_ascii_uppercase();
    [
        Path::new(cwd).join("HANDOFF.md"),
        Path::new(cwd).join(format!("{upper}-HANDOFF.md")),
    ]
    .into_iter()
    .find_map(|p| {
        let modified = std::fs::metadata(&p).ok()?.modified().ok()?;
        Some((
            p.to_string_lossy().into_owned(),
            DateTime::<Utc>::from(modified),
        ))
    })
}

/// Builds a spec from a launch description. Refuses inline config values. Does NOT check
/// completeness: `write_atomic` does, so a caller cannot write an incomplete one by skipping it.
pub fn build(i: &SpecInputs) -> Result<SpecFile, SpecError> {
    if !valid_identifier(&i.launch.role) {
        return Err(SpecError::InvalidRole(i.launch.role.clone()));
    }
    let (extras, _dropped) = extra_launch_args(i.launch.launch_args.as_deref().unwrap_or_default());
    for pair in extras.windows(2) {
        if INLINE_CAPABLE_FLAGS.contains(&pair[0].as_str()) && pair[1].trim_start().starts_with('{')
        {
            return Err(SpecError::InlineConfig(pair[0].clone()));
        }
    }
    let handoff = find_handoff(&i.launch.cwd, &i.launch.role);
    Ok(SpecFile {
        schema: SCHEMA,
        role: i.launch.role.clone(),
        name: i.launch.name.clone(),
        cwd: i.launch.cwd.clone(),
        model: i.model.to_string(),
        permission_mode: i.launch.permission_mode.clone(),
        remote_control: i.launch.remote_control,
        launch_args: extras,
        handoff_path: handoff.as_ref().map(|(p, _)| p.clone()),
        handoff_mtime: handoff.map(|(_, t)| t),
        state: i.state,
        written_at: i.now,
        written_by: i.written_by.to_string(),
        stopped_session_id: i.session_id.map(str::to_string),
        stopped_pid_start_secs: i.pid_start_secs,
        stop_reason: None,
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

impl SpecFile {
    /// The `LaunchSpec` a start would use. Its original-argv slot carries the explicit model and
    /// the allowlisted extras, so `resolve_launch_model` reads the model from the spec itself.
    pub fn launch_spec(&self) -> LaunchSpec {
        let mut argv = vec![
            "claude".to_string(),
            "--model".to_string(),
            self.model.clone(),
        ];
        argv.extend(self.launch_args.iter().cloned());
        LaunchSpec {
            role: self.role.clone(),
            name: self.name.clone(),
            cwd: self.cwd.clone(),
            permission_mode: self.permission_mode.clone(),
            remote_control: self.remote_control,
            launch_args: Some(argv),
        }
    }

    /// The spec with the model `lane-start` would launch: an explicit Sonnet or Haiku stays,
    /// anything else (Opus, unknown, unset) becomes `sonnet`. The second value is the model it
    /// replaced, for the ledger.
    pub fn pinned(&self) -> (SpecFile, Option<String>) {
        let (model, was) = pin_model(&self.model);
        let mut s = self.clone();
        s.model = model;
        (s, was)
    }

    /// Complete means: `prepare_launch` accepts it (plain role and name, a non-Opus explicit
    /// model, no `;` in any element). Opus is never allowed here.
    pub fn check(&self) -> Result<PreparedLaunch, SpecError> {
        if self.schema != SCHEMA {
            return Err(SpecError::UnsupportedSchema(self.schema));
        }
        if self.model.trim().is_empty() {
            return Err(SpecError::Incomplete("no model".to_string()));
        }
        prepare_launch(&self.launch_spec(), &self.model, false)
            .map_err(|e: RelaunchError| SpecError::Incomplete(e.to_string()))
    }
}

/// CireSnave, 2026-10-08: "I can't afford Opus." Only an explicit Sonnet or Haiku is launched;
/// everything else is replaced by `sonnet`, and the replaced value is returned.
pub fn pin_model(model: &str) -> (String, Option<String>) {
    let lower = model.to_ascii_lowercase();
    let ok = !lower.contains("opus") && (lower.contains("sonnet") || lower.contains("haiku"));
    if ok {
        (model.to_string(), None)
    } else {
        ("sonnet".to_string(), Some(model.to_string()))
    }
}

fn spec_path(dir: &Path, role: &str) -> Result<PathBuf, SpecError> {
    if !valid_identifier(role) {
        return Err(SpecError::InvalidRole(role.to_string()));
    }
    Ok(dir.join(format!("{role}{FILE_SUFFIX}")))
}

pub fn read(dir: &Path, role: &str) -> Result<SpecFile, SpecError> {
    let path = spec_path(dir, role)?;
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(SpecError::NotFound(path.display().to_string()))
        }
        Err(e) => return Err(SpecError::Unreadable(e.to_string())),
    };
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| SpecError::Malformed(e.to_string()))?;
    match value.get("schema").and_then(|s| s.as_u64()) {
        Some(n) if n == u64::from(SCHEMA) => {}
        Some(n) => {
            return Err(SpecError::UnsupportedSchema(
                n.min(u64::from(u32::MAX)) as u32
            ))
        }
        None => return Err(SpecError::Malformed("no schema field".to_string())),
    }
    serde_json::from_value(value).map_err(|e| SpecError::Malformed(e.to_string()))
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes `spec` durably: complete (`check`), to a temp file in the same directory, `sync_all`,
/// atomic rename over the spec, then read back and compared. Any failure leaves the previous
/// spec (or none) in place and removes the temp file. A reader sees the old file or the new one,
/// never half of one.
pub fn write_atomic(dir: &Path, spec: &SpecFile) -> Result<(), SpecError> {
    spec.check()?;
    let path = spec_path(dir, &spec.role)?;
    std::fs::create_dir_all(dir).map_err(|e| SpecError::Write(e.to_string()))?;
    sweep_temp_files(dir, &spec.role);
    let tmp = dir.join(format!(
        "{}{FILE_SUFFIX}.tmp-{}-{}",
        spec.role,
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> Result<(), SpecError> {
        let bytes = serde_json::to_vec_pretty(spec).map_err(|e| SpecError::Write(e.to_string()))?;
        let mut f = std::fs::File::create(&tmp).map_err(|e| SpecError::Write(e.to_string()))?;
        std::io::Write::write_all(&mut f, &bytes).map_err(|e| SpecError::Write(e.to_string()))?;
        f.sync_all().map_err(|e| SpecError::Write(e.to_string()))?;
        drop(f);
        std::fs::rename(&tmp, &path).map_err(|e| SpecError::Write(e.to_string()))?;
        if read(dir, &spec.role)? != *spec {
            return Err(SpecError::ReadBackMismatch);
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn sweep_temp_files(dir: &Path, role: &str) {
    let prefix = format!("{role}{FILE_SUFFIX}.tmp-");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with(&prefix) {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > TEMP_SWEEP_AFTER);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Where a restart persists specs. The real one is `DirSpecStore`; tests record.
pub trait SpecStore {
    fn write(&self, spec: &SpecFile) -> Result<(), SpecError>;
}

pub struct DirSpecStore {
    pub dir: PathBuf,
}

impl SpecStore for DirSpecStore {
    fn write(&self, spec: &SpecFile) -> Result<(), SpecError> {
        write_atomic(&self.dir, spec)
    }
}

/// Persists nothing. For callers that do not stop a lane (and for older tests); the real restart
/// path (`relaunch::kill_and_relaunch`) never uses it.
pub struct NoSpecStore;

impl SpecStore for NoSpecStore {
    fn write(&self, _: &SpecFile) -> Result<(), SpecError> {
        Ok(())
    }
}

// ---- lifecycle (RESTART-TOOL-DESIGN.md 14.2) -------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Running,
    Suspended,
    Crashed,
    NeverRun,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecStatus {
    Complete,
    Missing,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RoleStatus {
    pub role: String,
    pub state: Lifecycle,
    pub spec: SpecStatus,
    /// A spec exists, passes `check`, and no matching process is live.
    pub startable: bool,
    pub since: Option<DateTime<Utc>>,
    pub stop_reason: Option<String>,
    pub detail: Option<String>,
    /// The model the launch spec records (`None` when there is no readable spec). Opus is shown as
    /// recorded; a start replaces it (see `lane-restart models`).
    pub model: Option<String>,
    /// What the role's own hooks last recorded, present only when the recorded process is live
    /// (pid and start time): a dead process's last write says nothing about it now, so a lane
    /// killed mid-turn cannot read as busy. Absent for every other role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity: Option<Activity>,
    /// Size of the role's HANDOFF file against its ceiling (8 KB lane, 20 KB PM), read from the
    /// role's recorded cwd. `None` when no cwd is known or there is no such file: "none", never
    /// 0 bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff: Option<HandoffSize>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HandoffSize {
    pub path: String,
    pub bytes: u64,
    pub ceiling: u64,
    pub over: bool,
}

/// The lane's own record of its activity (`LaneState`), raw, plus the one idle verdict.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Activity {
    pub busy: bool,
    pub subagents_running: u32,
    /// `None`: the lane never asserted it. Not the same as `false`.
    pub no_background_shells: Option<bool>,
    pub updated_at: DateTime<Utc>,
    pub updated_by_event: String,
    /// `LaneState::is_idle`: not busy, no subagents, shells asserted absent. It does NOT include
    /// the PM's fourth term, "no unread TASK", which no state file records.
    pub idle: bool,
    /// Which terms of `idle` fail (empty when `idle`); see `LaneState::idle_blockers`.
    pub idle_blockers: Vec<String>,
    /// Seconds since the lane's hooks last wrote this record, at the time of the read. A live,
    /// not-busy process with a large value is the quiet stall no hook event announces. No
    /// threshold is applied here: the number is the whole claim.
    pub quiet_secs: i64,
}

fn handoff_size(cwd: &str, role: &str) -> Option<HandoffSize> {
    let (path, _) = find_handoff(cwd, role)?;
    let bytes = std::fs::metadata(&path).ok()?.len();
    let ceiling = crate::handoff_lint::ceiling(role);
    Some(HandoffSize {
        path,
        bytes,
        ceiling,
        over: bytes > ceiling,
    })
}

/// Is the process a state file names still the same live `claude`? Pid AND start time; a pid
/// whose start time was never recorded counts as alive if a claude holds it (never start a second
/// copy on a doubt).
pub(crate) fn state_process_is_live(st: &state::LaneState, facts: &dyn SystemFacts) -> bool {
    if !facts.is_alive_claude_process(st.pid) {
        return false;
    }
    match (st.pid_start_secs, facts.process_identity(st.pid)) {
        (Some(recorded), Some(id)) => id.start_time_secs == recorded,
        (Some(_), None) => false,
        (None, _) => true,
    }
}

/// Liveness comes from the process table, never from a file's age (14.2).
pub fn role_status(dir: &Path, role: &str, facts: &dyn SystemFacts) -> RoleStatus {
    let mut out = RoleStatus {
        role: role.to_string(),
        state: Lifecycle::Unknown,
        spec: SpecStatus::Missing,
        startable: false,
        since: None,
        stop_reason: None,
        detail: None,
        model: None,
        activity: None,
        handoff: None,
    };
    let lane_state = state::load(dir, role);
    let live = lane_state
        .as_ref()
        .is_ok_and(|st| state_process_is_live(st, facts));
    if live {
        if let Ok(st) = &lane_state {
            out.activity = Some(Activity {
                busy: st.busy,
                subagents_running: st.subagents_running,
                no_background_shells: st.no_background_shells,
                updated_at: st.updated_at,
                updated_by_event: st.updated_by_event.clone(),
                idle: st.is_idle(),
                idle_blockers: st.idle_blockers(),
                quiet_secs: (facts.now() - st.updated_at).num_seconds().max(0),
            });
        }
    }
    if let Ok(st) = &lane_state {
        out.handoff = handoff_size(&st.cwd, role);
    }
    let spec = read(dir, role);
    let mut spec_state = None;
    match &spec {
        Ok(s) => {
            out.since = Some(s.written_at);
            if out.handoff.is_none() {
                out.handoff = handoff_size(&s.cwd, role);
            }
            out.stop_reason = s.stop_reason.clone();
            out.model = Some(s.model.clone());
            // `lane-start` replaces an Opus, unknown or unset model by `sonnet` (14.5), so the
            // spec is startable when it is complete with that replacement applied.
            match s.pinned().0.check() {
                Ok(_) => out.spec = SpecStatus::Complete,
                Err(e) => {
                    out.spec = SpecStatus::Invalid;
                    out.detail = Some(e.to_string());
                }
            }
            spec_state = Some(s.state);
        }
        Err(SpecError::NotFound(_)) => {}
        Err(e) => {
            out.spec = SpecStatus::Invalid;
            out.detail = Some(e.to_string());
            out.state = if live {
                Lifecycle::Running
            } else {
                Lifecycle::Unknown
            };
            return out;
        }
    }
    if live {
        out.state = Lifecycle::Running;
        return out;
    }
    match (spec_state, &lane_state) {
        (Some(SpecState::Suspended), _) => out.state = Lifecycle::Suspended,
        (Some(SpecState::Running | SpecState::Stopping | SpecState::Crashed), _) => {
            out.state = Lifecycle::Crashed
        }
        (None, Ok(st)) => {
            out.state = Lifecycle::Crashed;
            out.since = Some(st.updated_at);
            out.detail = Some(
                "no launch spec: it is written at the next launch by lane-restart".to_string(),
            );
        }
        (None, Err(state::LoadError::NotFound(_))) => out.state = Lifecycle::NeverRun,
        (None, Err(e)) => out.detail = Some(e.to_string()),
    }
    out.startable = out.spec == SpecStatus::Complete;
    out
}

#[derive(Debug, Serialize)]
pub struct StatusReport {
    pub schema: u32,
    pub roles: Vec<RoleStatus>,
    /// `*.json` files that are neither a lane state nor a launch spec (listed, never dropped).
    pub skipped: Vec<String>,
    /// Free space on the state volume against the launch floor (added, same schema: a reader that
    /// ignores unknown fields is unaffected).
    pub disk: crate::disk::DiskStatus,
    /// How many roles have a complete launch spec, and why each of the others does not (added,
    /// same schema).
    pub coverage: SpecCoverage,
}

/// Roles that share one reason for having no complete launch spec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SpecGap {
    pub why: String,
    pub roles: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SpecCoverage {
    pub total: usize,
    pub complete: usize,
    /// Every role that is not complete appears in exactly one gap; gaps are sorted by reason.
    pub gaps: Vec<SpecGap>,
}

/// Why `role` has no complete launch spec, in terms of what `specs import` would do with it
/// (`import_one`: launch_args recorded, cwd a directory).
fn gap_reason(dir: &Path, r: &RoleStatus) -> String {
    if r.spec == SpecStatus::Invalid {
        return format!(
            "launch spec invalid: {}",
            r.detail.as_deref().unwrap_or("unknown")
        );
    }
    match state::load(dir, &r.role) {
        Err(_) => "no lane state and no launch spec".to_string(),
        Ok(st) if st.launch_args.as_ref().is_none_or(Vec::is_empty) => {
            "no launch spec; the state records no launch_args (not importable)".to_string()
        }
        Ok(st) if !Path::new(&st.cwd).is_dir() => {
            "no launch spec; cwd missing (import refuses it: recreate the directory, or `specs retire`)"
                .to_string()
        }
        Ok(_) => "no launch spec; importable with `specs import`".to_string(),
    }
}

/// Pure over the report plus a reason per incomplete role.
pub fn coverage(roles: &[RoleStatus], reason: impl Fn(&RoleStatus) -> String) -> SpecCoverage {
    let mut by_why: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut complete = 0;
    for r in roles {
        if r.spec == SpecStatus::Complete {
            complete += 1;
        } else {
            by_why.entry(reason(r)).or_default().push(r.role.clone());
        }
    }
    SpecCoverage {
        total: roles.len(),
        complete,
        gaps: by_why
            .into_iter()
            .map(|(why, roles)| SpecGap { why, roles })
            .collect(),
    }
}

/// Every role with a lane state or a launch spec under `dir`.
pub fn status_all(dir: &Path, facts: &dyn SystemFacts) -> StatusReport {
    let mut roles = std::collections::BTreeSet::new();
    let mut skipped = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(stem) = name.strip_suffix(".json") else {
                continue;
            };
            if let Some(role) = stem.strip_suffix(".launch") {
                if valid_identifier(role) {
                    roles.insert(role.to_string());
                    continue;
                }
            }
            if valid_identifier(stem) && state::load(dir, stem).is_ok() {
                roles.insert(stem.to_string());
            } else {
                skipped.push(name);
            }
        }
    }
    skipped.sort();
    let roles: Vec<RoleStatus> = roles.iter().map(|r| role_status(dir, r, facts)).collect();
    let coverage = coverage(&roles, |r| gap_reason(dir, r));
    StatusReport {
        schema: 1,
        roles,
        skipped,
        disk: crate::disk::status(dir),
        coverage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{log, RecordingFacts};
    use chrono::TimeZone;
    use tempfile::tempdir;

    fn launch(role: &str) -> LaunchSpec {
        LaunchSpec {
            role: role.to_string(),
            name: role.to_string(),
            cwd: "C:/Projects/x".to_string(),
            permission_mode: Some("auto".to_string()),
            remote_control: false,
            launch_args: Some(
                [
                    "claude.exe",
                    "prompt",
                    "--name",
                    role,
                    "--model",
                    "sonnet",
                    "--dangerously-load-development-channels",
                    "server:claude-peers",
                ]
                .map(String::from)
                .to_vec(),
            ),
        }
    }

    fn inputs<'a>(l: &'a LaunchSpec, model: &'a str, state: SpecState) -> SpecInputs<'a> {
        SpecInputs {
            launch: l,
            model,
            session_id: Some("sess-1"),
            pid_start_secs: Some(1234),
            state,
            written_by: "restart",
            now: Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap(),
        }
    }

    fn built(role: &str, state: SpecState) -> SpecFile {
        build(&inputs(&launch(role), "sonnet", state)).unwrap()
    }

    #[test]
    fn a_spec_round_trips_through_the_atomic_write() {
        let d = tempdir().unwrap();
        let s = built("agentlife", SpecState::Stopping);
        write_atomic(d.path(), &s).unwrap();
        assert_eq!(read(d.path(), "agentlife").unwrap(), s);
        assert_eq!(
            s.launch_args,
            vec![
                "--dangerously-load-development-channels",
                "server:claude-peers"
            ]
        );
    }

    #[test]
    fn a_write_replaces_the_old_spec_and_leaves_no_temp_file() {
        let d = tempdir().unwrap();
        write_atomic(d.path(), &built("agentlife", SpecState::Running)).unwrap();
        write_atomic(d.path(), &built("agentlife", SpecState::Suspended)).unwrap();
        assert_eq!(
            read(d.path(), "agentlife").unwrap().state,
            SpecState::Suspended
        );
        let names: Vec<String> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["agentlife.launch.json"]);
    }

    #[test]
    fn an_opus_spec_is_refused_and_nothing_is_written() {
        let d = tempdir().unwrap();
        let l = launch("agentlife");
        let s = build(&inputs(&l, "claude-opus-5-5", SpecState::Stopping)).unwrap();
        assert!(matches!(
            write_atomic(d.path(), &s),
            Err(SpecError::Incomplete(_))
        ));
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_semicolon_in_the_cwd_is_refused_before_anything_is_written() {
        let d = tempdir().unwrap();
        let mut l = launch("agentlife");
        l.cwd = "C:/a;b".to_string();
        let s = build(&inputs(&l, "sonnet", SpecState::Stopping)).unwrap();
        assert!(write_atomic(d.path(), &s).is_err());
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0);
    }

    #[test]
    fn inline_json_config_is_never_stored() {
        let mut l = launch("agentlife");
        l.launch_args = Some(
            [
                "claude.exe",
                "--mcp-config",
                r#"{"mcpServers":{"x":{"env":{"TOKEN":"s3cr3t"}}}}"#,
            ]
            .map(String::from)
            .to_vec(),
        );
        assert_eq!(
            build(&inputs(&l, "sonnet", SpecState::Stopping)).unwrap_err(),
            SpecError::InlineConfig("--mcp-config".to_string())
        );
    }

    #[test]
    fn a_config_file_path_is_stored_as_given() {
        let mut l = launch("agentlife");
        l.launch_args = Some(
            ["claude.exe", "--mcp-config", "C:/Projects/mcp.json"]
                .map(String::from)
                .to_vec(),
        );
        let s = build(&inputs(&l, "sonnet", SpecState::Stopping)).unwrap();
        assert_eq!(s.launch_args, vec!["--mcp-config", "C:/Projects/mcp.json"]);
    }

    #[test]
    fn an_unknown_flag_and_the_original_prompt_are_not_stored() {
        let mut l = launch("agentlife");
        l.launch_args = Some(
            [
                "claude.exe",
                "read agentlife HANDOFF and continue",
                "--mystery-flag",
                "--model",
                "sonnet",
            ]
            .map(String::from)
            .to_vec(),
        );
        let d = tempdir().unwrap();
        let s = build(&inputs(&l, "sonnet", SpecState::Stopping)).unwrap();
        write_atomic(d.path(), &s).unwrap();
        let bytes = std::fs::read_to_string(d.path().join("agentlife.launch.json")).unwrap();
        assert!(!bytes.contains("mystery"), "{bytes}");
        assert!(!bytes.contains("HANDOFF and continue"), "{bytes}");
    }

    #[test]
    fn a_spec_is_never_mistaken_for_a_lane_state() {
        let d = tempdir().unwrap();
        write_atomic(d.path(), &built("agentlife", SpecState::Running)).unwrap();
        // the stem `all_states` and `read_states` would derive from the file name
        assert!(matches!(
            state::load(d.path(), "agentlife.launch"),
            Err(state::LoadError::Malformed(_))
        ));
        assert!(is_spec_file_name("agentlife.launch.json"));
        assert!(!is_spec_file_name("agentlife.json"));
    }

    #[test]
    fn a_newer_schema_is_named_and_not_defaulted() {
        let d = tempdir().unwrap();
        std::fs::write(
            d.path().join("agentlife.launch.json"),
            r#"{"schema": 2, "role": "agentlife"}"#,
        )
        .unwrap();
        assert_eq!(
            read(d.path(), "agentlife").unwrap_err(),
            SpecError::UnsupportedSchema(2)
        );
    }

    #[test]
    fn a_role_that_is_a_path_is_refused() {
        let d = tempdir().unwrap();
        assert!(matches!(
            read(d.path(), "../x"),
            Err(SpecError::InvalidRole(_))
        ));
    }

    // ---- lifecycle -----------------------------------------------------------------------

    fn lane_state_file(dir: &Path, role: &str, pid: u32, start: Option<u64>) {
        let mut st = crate::testing::state_at(role, "s", "Stop");
        st.pid = pid;
        st.pid_start_secs = start;
        std::fs::write(
            dir.join(format!("{role}.json")),
            serde_json::to_string(&st).unwrap(),
        )
        .unwrap();
    }

    fn facts_with_live(pid: u32, start: u64) -> RecordingFacts {
        let mut f = RecordingFacts::new(&log());
        f.live_pid = Some((pid, start));
        f
    }

    fn dead_facts() -> RecordingFacts {
        RecordingFacts::new(&log())
    }

    #[test]
    fn a_role_with_nothing_on_disk_is_never_run_and_not_startable() {
        let d = tempdir().unwrap();
        let s = role_status(d.path(), "ghost", &dead_facts());
        assert_eq!((s.state, s.startable), (Lifecycle::NeverRun, false));
    }

    #[test]
    fn coverage_groups_every_incomplete_role_under_one_reason() {
        let d = tempdir().unwrap();
        let alive_dir = tempdir().unwrap();
        // complete spec
        write_atomic(d.path(), &built("done", SpecState::Suspended)).unwrap();
        // state, launch_args, cwd exists: importable
        let mut st = crate::testing::state_at("imp", "s", "PreToolUse");
        st.cwd = alive_dir.path().to_string_lossy().into_owned();
        st.launch_args = Some(vec!["claude.exe".into(), "--model".into(), "sonnet".into()]);
        std::fs::write(
            d.path().join("imp.json"),
            serde_json::to_string(&st).unwrap(),
        )
        .unwrap();
        // state, launch_args, cwd gone
        let mut gone = crate::testing::state_at("gone1", "s", "PreToolUse");
        gone.cwd = alive_dir.path().join("nope").to_string_lossy().into_owned();
        gone.launch_args = st.launch_args.clone();
        let mut gone2 = gone.clone();
        gone2.role = "gone2".into();
        std::fs::write(
            d.path().join("gone1.json"),
            serde_json::to_string(&gone).unwrap(),
        )
        .unwrap();
        std::fs::write(
            d.path().join("gone2.json"),
            serde_json::to_string(&gone2).unwrap(),
        )
        .unwrap();
        // state without launch_args
        let mut bare = crate::testing::state_at("bare", "s", "PreToolUse");
        bare.cwd = alive_dir.path().to_string_lossy().into_owned();
        bare.launch_args = None;
        std::fs::write(
            d.path().join("bare.json"),
            serde_json::to_string(&bare).unwrap(),
        )
        .unwrap();
        let r = status_all(d.path(), &dead_facts());
        let c = &r.coverage;
        assert_eq!((c.total, c.complete), (5, 1));
        let find = |needle: &str| {
            c.gaps
                .iter()
                .find(|g| g.why.contains(needle))
                .unwrap_or_else(|| panic!("no gap containing {needle}: {c:?}"))
        };
        assert_eq!(find("cwd missing").roles, vec!["gone1", "gone2"]);
        assert_eq!(find("importable with").roles, vec!["imp"]);
        assert_eq!(find("no launch_args").roles, vec!["bare"]);
        // every incomplete role is in exactly one gap
        let n: usize = c.gaps.iter().map(|g| g.roles.len()).sum();
        assert_eq!(n, c.total - c.complete);
    }

    #[test]
    fn status_carries_the_specs_model_and_the_disk_in_the_json() {
        let d = tempdir().unwrap();
        // No spec: no model. With a spec: the model it records (the `built` fixture is sonnet).
        assert_eq!(role_status(d.path(), "ghost", &dead_facts()).model, None);
        write_atomic(d.path(), &built("baracuda", SpecState::Suspended)).unwrap();
        let report = status_all(d.path(), &dead_facts());
        let v = serde_json::to_value(&report).unwrap();
        assert_eq!(v["roles"][0]["role"], "baracuda");
        assert_eq!(v["roles"][0]["model"], "sonnet");
        assert!(v["disk"]["floor_bytes"].as_u64().unwrap() > 0, "{v}");
        assert!(v["disk"].get("below_floor").is_some(), "{v}");
        assert_eq!(v["schema"], 1);
    }

    #[test]
    fn a_suspended_spec_with_no_process_is_startable() {
        let d = tempdir().unwrap();
        write_atomic(d.path(), &built("baracuda", SpecState::Suspended)).unwrap();
        let s = role_status(d.path(), "baracuda", &dead_facts());
        assert_eq!(
            (s.state, s.spec, s.startable),
            (Lifecycle::Suspended, SpecStatus::Complete, true)
        );
    }

    /// The baracuda case, 2026-10-11: a lane that ended mid-turn. Its state file is hours old
    /// and its pid is gone; the last-launch spec makes it startable, where the 120 s staleness
    /// refusal had no answer.
    #[test]
    fn a_crashed_lane_with_a_last_launch_spec_is_startable_whatever_the_age() {
        let d = tempdir().unwrap();
        let mut st = crate::testing::state_at("baracuda", "s", "PreToolUse");
        st.busy = true;
        st.pid = 19184;
        st.updated_at = Utc::now() - chrono::Duration::seconds(4775);
        std::fs::write(
            d.path().join("baracuda.json"),
            serde_json::to_string(&st).unwrap(),
        )
        .unwrap();
        write_atomic(d.path(), &built("baracuda", SpecState::Running)).unwrap();
        let s = role_status(d.path(), "baracuda", &dead_facts());
        assert_eq!(
            (s.state, s.spec, s.startable),
            (Lifecycle::Crashed, SpecStatus::Complete, true)
        );
    }

    #[test]
    fn a_crashed_lane_with_no_spec_is_named_and_not_startable() {
        let d = tempdir().unwrap();
        lane_state_file(d.path(), "baracuda", 19184, Some(100));
        let s = role_status(d.path(), "baracuda", &dead_facts());
        assert_eq!(
            (s.state, s.spec, s.startable),
            (Lifecycle::Crashed, SpecStatus::Missing, false)
        );
        assert!(s.detail.unwrap().contains("no launch spec"));
    }

    #[test]
    fn a_live_process_wins_over_a_suspended_spec() {
        let d = tempdir().unwrap();
        write_atomic(d.path(), &built("baracuda", SpecState::Suspended)).unwrap();
        lane_state_file(d.path(), "baracuda", 55, Some(100));
        let s = role_status(d.path(), "baracuda", &facts_with_live(55, 100));
        assert_eq!((s.state, s.startable), (Lifecycle::Running, false));
    }

    fn write_activity(dir: &Path, busy: bool, subagents: u32, shells: Option<bool>) {
        let mut st = crate::testing::state_at("baracuda", "s", "PreToolUse");
        st.pid = 55;
        st.pid_start_secs = Some(100);
        st.busy = busy;
        st.subagents_running = subagents;
        st.no_background_shells = shells;
        std::fs::write(
            dir.join("baracuda.json"),
            serde_json::to_string(&st).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn a_live_role_reports_its_recorded_activity_and_the_idle_verdict() {
        let d = tempdir().unwrap();
        write_activity(d.path(), false, 0, Some(true));
        let a = role_status(d.path(), "baracuda", &facts_with_live(55, 100))
            .activity
            .expect("live role has activity");
        assert_eq!(
            (
                a.busy,
                a.subagents_running,
                a.idle,
                a.updated_by_event.as_str()
            ),
            (false, 0, true, "PreToolUse")
        );
        for (busy, subs, shells) in [
            (true, 0, Some(true)),
            (false, 2, Some(true)),
            (false, 0, Some(false)),
            (false, 0, None),
        ] {
            write_activity(d.path(), busy, subs, shells);
            let a = role_status(d.path(), "baracuda", &facts_with_live(55, 100))
                .activity
                .unwrap();
            assert!(!a.idle, "busy={busy} subs={subs} shells={shells:?}");
        }
    }

    #[test]
    fn a_not_idle_live_role_names_the_failing_terms_and_how_long_it_has_been_quiet() {
        let d = tempdir().unwrap();
        let mut st = crate::testing::state_at("baracuda", "s", "Stop");
        st.pid = 55;
        st.pid_start_secs = Some(100);
        st.no_background_shells = None;
        st.updated_at = Utc::now() - chrono::Duration::seconds(7200);
        std::fs::write(
            d.path().join("baracuda.json"),
            serde_json::to_string(&st).unwrap(),
        )
        .unwrap();
        let a = role_status(d.path(), "baracuda", &facts_with_live(55, 100))
            .activity
            .unwrap();
        assert_eq!(a.idle_blockers, ["shells_never_asserted"]);
        assert!(!a.idle);
        // `RecordingFacts::now` is its own clock: the age is only ever non-negative and >= 7200
        // when that clock is at or after the write, which `Utc::now()` is.
        assert!(a.quiet_secs >= 0, "{}", a.quiet_secs);
    }

    #[test]
    fn handoff_size_is_read_from_the_recorded_cwd_and_absence_is_none_not_zero() {
        let d = tempdir().unwrap();
        let cwd = tempdir().unwrap();
        let mut st = crate::testing::state_at("baracuda", "s", "Stop");
        st.pid = 55;
        st.cwd = cwd.path().to_string_lossy().into_owned();
        std::fs::write(
            d.path().join("baracuda.json"),
            serde_json::to_string(&st).unwrap(),
        )
        .unwrap();
        assert_eq!(
            role_status(d.path(), "baracuda", &dead_facts()).handoff,
            None
        );
        std::fs::write(cwd.path().join("HANDOFF.md"), "a".repeat(9000)).unwrap();
        let h = role_status(d.path(), "baracuda", &dead_facts())
            .handoff
            .unwrap();
        assert_eq!((h.bytes, h.ceiling, h.over), (9000, 8192, true));
        std::fs::write(cwd.path().join("HANDOFF.md"), "a".repeat(100)).unwrap();
        let h = role_status(d.path(), "baracuda", &dead_facts())
            .handoff
            .unwrap();
        assert_eq!((h.bytes, h.over), (100, false));
    }

    /// A lane killed mid-turn leaves busy=true on disk; its process is gone, so nothing is reported.
    #[test]
    fn a_dead_process_reports_no_activity_even_if_it_died_busy() {
        let d = tempdir().unwrap();
        write_activity(d.path(), true, 3, Some(false));
        let s = role_status(d.path(), "baracuda", &dead_facts());
        assert_eq!(s.activity, None);
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("activity"), "{json}");
    }

    #[test]
    fn a_recycled_pid_does_not_make_a_lane_running() {
        let d = tempdir().unwrap();
        write_atomic(d.path(), &built("baracuda", SpecState::Running)).unwrap();
        lane_state_file(d.path(), "baracuda", 55, Some(100));
        // the pid is a claude again, but a different launch of it
        let s = role_status(d.path(), "baracuda", &facts_with_live(55, 999));
        assert_eq!((s.state, s.startable), (Lifecycle::Crashed, true));
    }

    #[test]
    fn a_newer_schema_spec_is_unknown_and_not_startable() {
        let d = tempdir().unwrap();
        std::fs::write(
            d.path().join("baracuda.launch.json"),
            r#"{"schema": 9, "role": "baracuda"}"#,
        )
        .unwrap();
        let s = role_status(d.path(), "baracuda", &dead_facts());
        assert_eq!((s.state, s.startable), (Lifecycle::Unknown, false));
    }

    #[test]
    fn status_all_lists_every_role_and_names_what_it_skipped() {
        let d = tempdir().unwrap();
        lane_state_file(d.path(), "live", 55, Some(100));
        write_atomic(d.path(), &built("suspended", SpecState::Suspended)).unwrap();
        std::fs::write(d.path().join("hf-config.json"), "{}").unwrap();
        let r = status_all(d.path(), &facts_with_live(55, 100));
        let roles: Vec<_> = r.roles.iter().map(|s| (s.role.as_str(), s.state)).collect();
        assert_eq!(
            roles,
            vec![
                ("live", Lifecycle::Running),
                ("suspended", Lifecycle::Suspended)
            ]
        );
        assert_eq!(r.skipped, vec!["hf-config.json"]);
    }
}
