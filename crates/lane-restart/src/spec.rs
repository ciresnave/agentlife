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
use crate::permission_mode::PermissionMode;
use crate::relaunch::{extra_launch_args, valid_identifier, RelaunchError};
use crate::role::Role;
use crate::state;
use crate::wsl::{self, WslTarget};
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
    /// A restart stopped the lane and the new session never came up (`relaunch FAILED`). The lane
    /// was killed to be started again, not to stay stopped, so it is a failure like `Crashed`
    /// rather than a stop: no `stop_kind` is needed and the launcher may start it (the retry).
    FailedRelaunch,
}

/// Why a lane was stopped, as far as a launcher may act on it (board 216, CireSnave: "216: yes").
///
/// `Soft`: the lane ended in the course of its own work (its own shutdown, idle or task-boundary
/// path), so a message for it may start it again. `Hard`: a person or the PM stopped it on purpose
/// and it stays stopped until a person or the PM starts it. A spec with **no** kind (older than this
/// field, or never stopped) is *unclassified* and is never started by the launcher.
///
/// Only the PM or CireSnave can turn a `Hard` stop into anything else; no lane can.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopKind {
    Soft,
    Hard,
}

impl StopKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StopKind::Soft => "soft",
            StopKind::Hard => "hard",
        }
    }
}

/// Who caused a spec write. On disk it is the plain string it always was; a spelling this build does
/// not know (an older or newer build's) reads as `Other` rather than refusing the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WrittenBy {
    Restart,
    Launch,
    Stop,
    Import,
    Unpark,
    Other(String),
}

impl WrittenBy {
    pub fn as_str(&self) -> &str {
        match self {
            WrittenBy::Restart => "restart",
            WrittenBy::Launch => "launch",
            WrittenBy::Stop => "stop",
            WrittenBy::Import => "import",
            WrittenBy::Unpark => "unpark",
            WrittenBy::Other(s) => s,
        }
    }
}

impl From<&str> for WrittenBy {
    fn from(s: &str) -> Self {
        match s {
            "restart" => WrittenBy::Restart,
            "launch" => WrittenBy::Launch,
            "stop" => WrittenBy::Stop,
            "import" => WrittenBy::Import,
            "unpark" => WrittenBy::Unpark,
            other => WrittenBy::Other(other.to_string()),
        }
    }
}

impl Serialize for WrittenBy {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for WrittenBy {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(String::deserialize(d)?.as_str().into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecFile {
    pub schema: u32,
    pub role: Role,
    pub name: Role,
    pub cwd: String,
    /// The explicit `--model` value a start passes.
    pub model: String,
    pub permission_mode: Option<PermissionMode>,
    pub remote_control: bool,
    /// Only what `relaunch::extra_launch_args` allows; never the raw argv.
    pub launch_args: Vec<String>,
    pub handoff_path: Option<String>,
    pub handoff_mtime: Option<DateTime<Utc>>,
    pub state: SpecState,
    pub written_at: DateTime<Utc>,
    /// Who caused this write.
    pub written_by: WrittenBy,
    /// The session this spec was derived from when it was stopped. Not named `session_id` or
    /// `pid` on purpose: see the module note on `LaneState`.
    pub stopped_session_id: Option<String>,
    pub stopped_pid_start_secs: Option<u64>,
    pub stop_reason: Option<String>,
    /// Soft or hard, written by every stop. `None` for a spec written by an older build, by a
    /// launch, or by `specs import` (a crash): unclassified, which a launcher refuses.
    #[serde(default)]
    pub stop_kind: Option<StopKind>,
    /// HMAC over the rest of the spec, written with every soft stop (`marker`). A soft stop without
    /// a MAC that verifies reads back as unclassified. Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_mac: Option<String>,
    pub tool_version: String,
    /// `Some("wsl")` for a lane that runs inside a WSL2 distro (docs/LANE-RESTART-WSL.md), which
    /// is also what makes the spec schema 2. Absent, and not written, for a Windows lane, so a
    /// Windows spec is byte-for-byte what it always was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<String>,
    /// The distro, user and wrapper of a `launch: wsl` spec; absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wsl: Option<WslTarget>,
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
    /// A `launch` value this build does not know. Never read as a Windows lane.
    UnsupportedLaunch(String),
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
                "the launch spec has schema {n}; this build reads schemas {SCHEMA} and {}",
                wsl::SCHEMA_WSL
            ),
            SpecError::UnsupportedLaunch(l) => write!(
                f,
                "the launch spec has launch mode {l:?}; this build knows only {:?}",
                wsl::LAUNCH_WSL
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
    pub written_by: WrittenBy,
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
    let (extras, _dropped) = extra_launch_args(i.launch.launch_args.as_deref().unwrap_or_default());
    for pair in extras.windows(2) {
        if INLINE_CAPABLE_FLAGS.contains(&pair[0].as_str()) && pair[1].trim_start().starts_with('{')
        {
            return Err(SpecError::InlineConfig(pair[0].clone()));
        }
    }
    let handoff = find_handoff(&i.launch.cwd, i.launch.role.as_str());
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
        written_by: i.written_by.clone(),
        stopped_session_id: i.session_id.map(str::to_string),
        stopped_pid_start_secs: i.pid_start_secs,
        stop_reason: None,
        stop_kind: None,
        stop_mac: None,
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        launch: None,
        wsl: None,
    })
}

impl SpecFile {
    /// This spec as a WSL lane (docs/LANE-RESTART-WSL.md section 2): schema 2, `launch: wsl`, the
    /// target. Not checked here; `check` and `write_atomic` do.
    pub fn with_wsl(mut self, target: WslTarget) -> SpecFile {
        self.schema = wsl::SCHEMA_WSL;
        self.launch = Some(wsl::LAUNCH_WSL.to_string());
        self.wsl = Some(target);
        self
    }

    /// The `wsl.exe` command a WSL spec launches (section 3): the checked claude argv wrapped for
    /// the distro. A Windows spec has none.
    pub fn wsl_argv(&self) -> Result<Vec<String>, SpecError> {
        let target = match (self.launch.as_deref(), &self.wsl) {
            (Some(wsl::LAUNCH_WSL), Some(t)) => t,
            _ => return Err(SpecError::Incomplete("not a wsl spec".to_string())),
        };
        let _ = target;
        let prepared = self.check()?;
        prepared
            .argv
            .wsl_command()
            .map(<[String]>::to_vec)
            .ok_or_else(|| SpecError::Incomplete("the launch is not a wsl launch".to_string()))
    }

    /// The schema and the launch fields agree (section 2): a Windows spec is schema 1 with no
    /// `launch` and no `wsl`; a WSL spec is schema 2, `launch: wsl`, a valid target and a POSIX
    /// `cwd`. Checked at write and at read, so a hand-edited file is not trusted.
    fn check_launch_fields(&self) -> Result<(), SpecError> {
        let incomplete = |why: &str| Err(SpecError::Incomplete(why.to_string()));
        match (self.schema, self.launch.as_deref(), self.wsl.as_ref()) {
            (SCHEMA, None, None) => Ok(()),
            (SCHEMA, _, _) => {
                incomplete("the launch fields need schema 2; a schema 1 spec is a Windows lane")
            }
            (wsl::SCHEMA_WSL, Some(wsl::LAUNCH_WSL), Some(target)) => target
                .validate()
                .and_then(|()| wsl::validate_cwd(&self.cwd))
                .map_err(|e| SpecError::Incomplete(e.to_string())),
            (wsl::SCHEMA_WSL, Some(wsl::LAUNCH_WSL), None) => {
                incomplete("launch is wsl but there is no wsl target")
            }
            (wsl::SCHEMA_WSL, Some(other), _) => {
                Err(SpecError::UnsupportedLaunch(other.to_string()))
            }
            (wsl::SCHEMA_WSL, None, _) => incomplete("schema 2 without a launch mode"),
            (n, _, _) => Err(SpecError::UnsupportedSchema(n)),
        }
    }

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
            wsl: self.wsl.clone(),
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
        self.check_launch_fields()?;
        if self.model.trim().is_empty() {
            return Err(SpecError::Incomplete("no model".to_string()));
        }
        prepare_launch(&self.launch_spec(), &self.model, None)
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

/// Whether `role`'s spec says `launch: wsl`. Any read problem (no spec, unreadable, malformed, a
/// schema this build does not know) is `false`: this is only a reason to refuse something extra,
/// never a reason to allow it.
pub fn is_wsl_lane(dir: &Path, role: &str) -> bool {
    read(dir, role).is_ok_and(|s| s.launch.as_deref() == Some(wsl::LAUNCH_WSL))
}

pub fn read(dir: &Path, role: &str) -> Result<SpecFile, SpecError> {
    read_raw(dir, role).map(|spec| verified(dir, spec))
}

/// `read` without the marker check: the file as written.
fn read_raw(dir: &Path, role: &str) -> Result<SpecFile, SpecError> {
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
        Some(n) if n == u64::from(SCHEMA) || n == u64::from(wsl::SCHEMA_WSL) => {}
        Some(n) => {
            return Err(SpecError::UnsupportedSchema(
                n.min(u64::from(u32::MAX)) as u32
            ))
        }
        None => return Err(SpecError::Malformed("no schema field".to_string())),
    }
    let spec: SpecFile =
        serde_json::from_value(value).map_err(|e| SpecError::Malformed(e.to_string()))?;
    spec.check_launch_fields()?;
    Ok(spec)
}

/// A soft stop is a grant to start the lane, so it counts only if its MAC verifies; one that does
/// not (edited, never signed, no key) is *unclassified*, which every launcher refuses.
fn verified(dir: &Path, mut spec: SpecFile) -> SpecFile {
    if spec.stop_kind == Some(StopKind::Soft) && !crate::marker::verify(dir, &spec) {
        spec.stop_kind = None;
    }
    spec
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes `spec` durably: complete (`check`), to a temp file in the same directory, `sync_all`,
/// atomic rename over the spec, then read back and compared. Any failure leaves the previous
/// spec (or none) in place and removes the temp file. A reader sees the old file or the new one,
/// never half of one.
pub fn write_atomic(dir: &Path, spec: &SpecFile) -> Result<(), SpecError> {
    spec.check()?;
    // A soft stop is signed here, the one place every spec is written (`marker`); no key means it
    // is written unsigned and reads back unclassified.
    let mut signed = spec.clone();
    signed.stop_mac = None;
    if signed.stop_kind == Some(StopKind::Soft) {
        signed.stop_mac = crate::marker::sign(dir, &signed);
    }
    let spec = &signed;
    let path = spec_path(dir, spec.role.as_str())?;
    std::fs::create_dir_all(dir).map_err(|e| SpecError::Write(e.to_string()))?;
    sweep_temp_files(dir, spec.role.as_str());
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
        if read_raw(dir, spec.role.as_str())? != *spec {
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
    /// The spec already recorded for `role`, when one reads and is complete. A restart keeps ITS
    /// launch flags (`specs both-channels` and the like edit them), not the running session's. A
    /// store with none says `None`.
    fn existing(&self, _role: &str) -> Option<SpecFile> {
        None
    }
}

pub struct DirSpecStore {
    pub dir: PathBuf,
}

impl SpecStore for DirSpecStore {
    fn write(&self, spec: &SpecFile) -> Result<(), SpecError> {
        write_atomic(&self.dir, spec)
    }
    fn existing(&self, role: &str) -> Option<SpecFile> {
        read(&self.dir, role)
            .ok()
            .filter(|s| s.role.as_str() == role && s.check().is_ok())
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
    /// The last restart stopped the lane and its replacement never came up; startable.
    FailedRelaunch,
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
    /// `soft` or `hard` when the spec records a stop; absent when unclassified.
    pub stop_kind: Option<StopKind>,
    pub detail: Option<String>,
    /// The model the launch spec records (`None` when there is no readable spec). Opus is shown as
    /// recorded; a start replaces it (see `lane-restart models`).
    pub model: Option<String>,
    /// What the role's own hooks last recorded, present only when the recorded process is live
    /// (pid and start time): a dead process's last write says nothing about it now, so a lane
    /// killed mid-turn cannot read as busy. Absent for every other role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity: Option<Activity>,
    /// Which of `--mcp-config`, `server:claude-peers` and `server:synapse` the LIVE session was launched
    /// with, read from the launch arguments its own state recorded. Absent for a role that is not live
    /// and for a state that recorded no launch arguments (shown as `?`, never as "none").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<crate::spec_channels::Channels>,
    /// Size of the role's HANDOFF file against its ceiling (8 KB lane, 20 KB PM), read from the
    /// role's recorded cwd. `None` when no cwd is known or there is no such file: "none", never
    /// 0 bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff: Option<HandoffSize>,
    /// The role whose recorded cwd contains this role's cwd (the deepest one), when there is one.
    /// A label only: it never changes `startable` or what `specs import` does. A session that
    /// `cd`-ed into a sub-directory records the sub-directory's leaf name as its role, so this is
    /// where such a role stands out; a legitimate nested lane (a worktree under its repo) carries
    /// it too, which is why it cannot be a refusal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nested_in: Option<String>,
    /// Only with `status --peers`: the broker peers whose helper's parent is this role's live
    /// `claude`, or `"unknown"` when the broker could not be read. Absent without the flag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peers: Option<crate::peers_join::RolePeers>,
    /// `duplicate-risk`: live `lane-restart host --role R` / claude processes of this role that
    /// its own live state file does not account for (a session stuck before its `SessionStart`,
    /// or one a stale state file hides). Empty - and absent from JSON - for a normal row.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub duplicate_risk: Vec<crate::duplicate::Duplicate>,
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
    /// Only with `status --quiet-over N`: whether `quiet_secs > N`. Absent without the flag, so the
    /// default output is unchanged. It is a mark on the raw number, not a stall verdict: a lane in
    /// one long tool call is also quiet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quiet_over: Option<bool>,
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
        stop_kind: None,
        detail: None,
        model: None,
        activity: None,
        channels: None,
        handoff: None,
        nested_in: None,
        peers: None,
        duplicate_risk: Vec::new(),
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
                quiet_over: None,
            });
            out.channels = st
                .launch_args
                .as_deref()
                .map(crate::spec_channels::Channels::of);
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
            out.stop_kind = s.stop_kind;
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
        (Some(SpecState::FailedRelaunch), _) => out.state = Lifecycle::FailedRelaunch,
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
    /// The `--quiet-over` threshold the `activity.quiet_over` marks were computed with; absent
    /// when the flag was not given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quiet_over_secs: Option<i64>,
    /// Only with `status --peers`: peers whose helper's parent is no role's live `claude`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peers_unmatched: Option<Vec<crate::peers_join::UnmatchedPeer>>,
    /// Only with `status --peers`: why the broker could not be read (every role is then `unknown`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peers_error: Option<String>,
    /// Why the process table could not be read for the `duplicate-risk` marks (none were made).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate_error: Option<String>,
}

impl StatusReport {
    /// Mark each live role's activity with whether it has been quiet longer than `secs`. Opt-in
    /// and never enforced: nothing in this tool acts on the mark.
    pub fn mark_quiet_over(&mut self, secs: i64) {
        self.quiet_over_secs = Some(secs);
        for r in &mut self.roles {
            if let Some(a) = &mut r.activity {
                a.quiet_over = Some(a.quiet_secs > secs);
            }
        }
    }
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

/// `/`-separated, lower-case, no trailing `/` and no `\\?\` prefix: the form two cwds are compared in
/// (Windows paths are case-insensitive and the state files mix `\` and `/`).
pub(crate) fn norm_cwd(p: &str) -> String {
    let p = p.replace('\\', "/");
    let p = p.strip_prefix("//?/").unwrap_or(&p);
    p.trim_end_matches('/').to_lowercase()
}

/// Pure: for each `(role, cwd)`, the role whose cwd is the deepest proper ancestor directory of
/// it. Equal cwds are not nesting (two roles in one directory is a different finding), and ties
/// between equally deep ancestors go to the first role by name. Only roles that are nested appear.
pub fn nesting(cwds: &[(String, String)]) -> std::collections::BTreeMap<String, String> {
    let norm: Vec<(&str, String)> = cwds
        .iter()
        .map(|(r, c)| (r.as_str(), norm_cwd(c)))
        .collect();
    let mut out = std::collections::BTreeMap::new();
    for (role, cwd) in &norm {
        if cwd.is_empty() {
            continue;
        }
        let parent = norm
            .iter()
            .filter(|(other, oc)| {
                other != role
                    && !oc.is_empty()
                    && oc != cwd
                    && cwd
                        .strip_prefix(oc.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            })
            .max_by(|a, b| a.1.len().cmp(&b.1.len()).then(b.0.cmp(a.0)));
        if let Some((p, _)) = parent {
            out.insert((*role).to_string(), (*p).to_string());
        }
    }
    out
}

#[derive(Debug, PartialEq, Eq)]
pub enum SelfRoleError {
    /// `LANE_ROLE` is set but is not a plain role identifier.
    BadEnv(String),
    /// No lane state records this cwd (and `LANE_ROLE` is unset).
    NoMatch(String),
    /// More than one lane state records this cwd, and the claude pid did not pick one.
    Ambiguous(Vec<String>),
}

impl std::fmt::Display for SelfRoleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadEnv(v) => write!(
                f,
                "LANE_ROLE={v:?} is not a plain role identifier; pass --role <name>"
            ),
            Self::NoMatch(cwd) => write!(
                f,
                "no lane state records the directory {cwd}; set LANE_ROLE or pass --role <name>"
            ),
            Self::Ambiguous(c) => write!(
                f,
                "more than one role records this directory ({}); pass --role <name>",
                c.join(", ")
            ),
        }
    }
}

/// Which role `--self` means when no `--role` was given: `LANE_ROLE` if set, else the one role
/// whose lane state records this `cwd`. When several do, the role whose recorded pid is
/// `claude_pid` breaks the tie; if that does not leave exactly one, the answer is an error naming
/// the candidates, never a guess. The restart's own checks (`authorize::decide`: the pid in the
/// state file must be the caller's claude) still run on the result, so this only saves typing.
pub fn resolve_self_role(
    dir: &Path,
    lane_role_env: Option<&str>,
    cwd: &str,
    claude_pid: Option<u32>,
) -> Result<String, SelfRoleError> {
    if let Some(v) = lane_role_env.map(str::trim).filter(|v| !v.is_empty()) {
        return if crate::relaunch::valid_identifier(v) {
            Ok(v.to_string())
        } else {
            Err(SelfRoleError::BadEnv(v.to_string()))
        };
    }
    let here = norm_cwd(cwd);
    let mut found: Vec<(String, u32)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if is_spec_file_name(&name) {
                continue;
            }
            let Some(stem) = name.strip_suffix(".json") else {
                continue;
            };
            if !crate::relaunch::valid_identifier(stem) {
                continue;
            }
            let Ok(st) = state::load(dir, stem) else {
                continue;
            };
            if st.role == stem && !here.is_empty() && norm_cwd(&st.cwd) == here {
                found.push((stem.to_string(), st.pid));
            }
        }
    }
    found.sort();
    if found.len() > 1 {
        if let Some(pid) = claude_pid {
            let by_pid: Vec<_> = found.iter().filter(|(_, p)| *p == pid).cloned().collect();
            if by_pid.len() == 1 {
                found = by_pid;
            }
        }
    }
    match found.len() {
        0 => Err(SelfRoleError::NoMatch(cwd.to_string())),
        1 => Ok(found.remove(0).0),
        _ => Err(SelfRoleError::Ambiguous(
            found.into_iter().map(|(r, _)| r).collect(),
        )),
    }
}

/// `nesting` over the cwd each of `roles` recorded in its lane state (a role with no readable
/// state contributes nothing).
pub fn nested_roles(dir: &Path, roles: &[String]) -> std::collections::BTreeMap<String, String> {
    let cwds: Vec<(String, String)> = roles
        .iter()
        .filter_map(|r| state::load(dir, r).ok().map(|st| (r.clone(), st.cwd)))
        .collect();
    nesting(&cwds)
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
    let names: Vec<String> = roles.iter().cloned().collect();
    let nested = nested_roles(dir, &names);
    let roles: Vec<RoleStatus> = roles
        .iter()
        .map(|r| {
            let mut s = role_status(dir, r, facts);
            s.nested_in = nested.get(r).cloned();
            s
        })
        .collect();
    let coverage = coverage(&roles, |r| gap_reason(dir, r));
    StatusReport {
        schema: 1,
        roles,
        skipped,
        disk: crate::disk::status(dir),
        coverage,
        quiet_over_secs: None,
        peers_unmatched: None,
        peers_error: None,
        duplicate_error: None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn written_by_is_the_plain_string_on_disk_and_an_unknown_one_still_reads() {
        for (w, text) in [
            (WrittenBy::Restart, "\"restart\""),
            (WrittenBy::Launch, "\"launch\""),
            (WrittenBy::Stop, "\"stop\""),
            (WrittenBy::Import, "\"import\""),
            (WrittenBy::Unpark, "\"unpark\""),
            (WrittenBy::Other("later".into()), "\"later\""),
        ] {
            assert_eq!(serde_json::to_string(&w).unwrap(), text);
            assert_eq!(serde_json::from_str::<WrittenBy>(text).unwrap(), w);
        }
        // A spelling a known variant would own never lands in `Other`.
        assert_eq!(WrittenBy::from("stop"), WrittenBy::Stop);
    }

    use super::*;
    use crate::testing::{log, RecordingFacts};
    use chrono::TimeZone;
    use tempfile::tempdir;

    fn launch(role: &str) -> LaunchSpec {
        LaunchSpec {
            wsl: None,
            role: Role::new(role).unwrap(),
            name: Role::new(role).unwrap(),
            cwd: "C:/Projects/x".to_string(),
            permission_mode: Some("auto".into()),
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
            written_by: WrittenBy::Restart,
            now: Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap(),
        }
    }

    // A spec written before the field existed is unclassified, never soft or hard.
    #[test]
    fn a_spec_without_stop_kind_reads_as_unclassified_and_a_kind_round_trips() {
        let mut v = serde_json::to_value(built("agentlife", SpecState::Suspended)).unwrap();
        assert!(v.as_object_mut().unwrap().remove("stop_kind").is_some());
        let old: SpecFile = serde_json::from_value(v).unwrap();
        assert_eq!(old.stop_kind, None);
        let mut s = built("agentlife", SpecState::Suspended);
        s.stop_kind = Some(StopKind::Soft);
        let back: SpecFile = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.stop_kind, Some(StopKind::Soft));
        assert_eq!(StopKind::Hard.as_str(), "hard");
    }

    #[test]
    fn a_spec_file_with_a_role_or_name_that_is_not_an_identifier_does_not_parse() {
        let good = serde_json::to_value(built("agentlife", SpecState::Suspended)).unwrap();
        assert!(serde_json::from_value::<SpecFile>(good.clone()).is_ok());
        for key in ["role", "name"] {
            let mut bad = good.clone();
            bad[key] = "a;calc".into();
            assert!(
                serde_json::from_value::<SpecFile>(bad).is_err(),
                "{key} accepted"
            );
        }
    }

    fn built(role: &str, state: SpecState) -> SpecFile {
        build(&inputs(&launch(role), "sonnet", state)).unwrap()
    }

    fn soft_stop(dir: &Path, role: &str, kind: Option<StopKind>) -> PathBuf {
        let mut s = built(role, SpecState::Suspended);
        s.stop_kind = kind;
        write_atomic(dir, &s).unwrap();
        spec_path(dir, role).unwrap()
    }

    #[test]
    fn a_written_soft_stop_is_signed_and_reads_back_soft() {
        let d = tempdir().unwrap();
        let path = soft_stop(d.path(), "alpha", Some(StopKind::Soft));
        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(on_disk["stop_mac"].is_string(), "{on_disk}");
        assert_eq!(
            read(d.path(), "alpha").unwrap().stop_kind,
            Some(StopKind::Soft)
        );
        // a hard stop and an unclassified one carry no MAC
        for (role, kind) in [("beta", Some(StopKind::Hard)), ("gamma", None)] {
            let p = soft_stop(d.path(), role, kind);
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
            assert!(v.get("stop_mac").is_none(), "{role}: {v}");
        }
    }

    #[test]
    fn an_edited_or_unsigned_soft_stop_reads_back_unclassified() {
        let d = tempdir().unwrap();
        // 1. hard -> soft by hand: no MAC
        let path = soft_stop(d.path(), "hard", Some(StopKind::Hard));
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("\"hard\"", "\"soft\"")).unwrap();
        let forged = read(d.path(), "hard").unwrap();
        assert_eq!(forged.stop_kind, None, "forged soft must not read as soft");
        // 2. a genuine soft stop whose cwd is then edited
        let path = soft_stop(d.path(), "alpha", Some(StopKind::Soft));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("C:/Projects/x"),
            "control: the anchor is present: {text}"
        );
        std::fs::write(&path, text.replace("C:/Projects/x", "C:/Projects/evil")).unwrap();
        assert_eq!(read(d.path(), "alpha").unwrap().stop_kind, None);
        // 3. a genuine soft stop copied to another directory (another key)
        let other = tempdir().unwrap();
        let src = soft_stop(d.path(), "delta", Some(StopKind::Soft));
        std::fs::copy(&src, spec_path(other.path(), "delta").unwrap()).unwrap();
        assert_eq!(read(other.path(), "delta").unwrap().stop_kind, None);
        // control: the same file in its own directory is still soft
        assert_eq!(
            read(d.path(), "delta").unwrap().stop_kind,
            Some(StopKind::Soft)
        );
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
            r#"{"schema": 3, "role": "agentlife"}"#,
        )
        .unwrap();
        assert_eq!(
            read(d.path(), "agentlife").unwrap_err(),
            SpecError::UnsupportedSchema(3)
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

    fn pair(r: &str, c: &str) -> (String, String) {
        (r.to_string(), c.to_string())
    }

    fn write_state(d: &Path, role: &str, cwd: &str, pid: u32) {
        let mut st = crate::testing::state_at(role, "s", "Stop");
        st.pid = pid;
        st.cwd = cwd.to_string();
        std::fs::write(
            d.join(format!("{role}.json")),
            serde_json::to_string(&st).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn self_role_is_lane_role_then_the_one_role_with_this_cwd_and_never_a_guess() {
        let d = tempdir().unwrap();
        write_state(d.path(), "agentlife", "C:\\Projects\\agentlife", 10);
        write_state(d.path(), "docs", "C:/Projects/agentlife/docs", 11);
        let r = |env: Option<&str>, cwd: &str, pid: Option<u32>| {
            resolve_self_role(d.path(), env, cwd, pid)
        };
        // LANE_ROLE wins, even over a cwd that matches another role
        assert_eq!(
            r(Some("docs"), "C:/Projects/agentlife", None),
            Ok("docs".into())
        );
        // else the role whose cwd matches (case and separator insensitive)
        assert_eq!(
            r(None, "c:/projects/AgentLife/", None),
            Ok("agentlife".into())
        );
        // a nested directory is its own role's cwd, not its parent's
        assert_eq!(
            r(None, "C:/Projects/agentlife/docs", None),
            Ok("docs".into())
        );
        assert!(matches!(
            r(None, "C:/Projects/other", None),
            Err(SelfRoleError::NoMatch(_))
        ));
        assert!(matches!(
            r(Some("../x"), "C:/Projects/agentlife", None),
            Err(SelfRoleError::BadEnv(_))
        ));
        // two roles, one cwd: the claude pid breaks the tie, otherwise it is refused naming both
        write_state(d.path(), "twin", "C:/Projects/agentlife", 12);
        assert_eq!(
            r(None, "C:/Projects/agentlife", Some(12)),
            Ok("twin".into())
        );
        let both = Err(SelfRoleError::Ambiguous(vec![
            "agentlife".into(),
            "twin".into(),
        ]));
        assert_eq!(r(None, "C:/Projects/agentlife", Some(99)), both);
        assert_eq!(r(None, "C:/Projects/agentlife", None), both);
        // a pid shared by two candidates narrows nothing: all three stay named
        write_state(d.path(), "twin2", "C:/Projects/agentlife", 12);
        assert_eq!(
            r(None, "C:/Projects/agentlife", Some(12)),
            Err(SelfRoleError::Ambiguous(vec![
                "agentlife".into(),
                "twin".into(),
                "twin2".into()
            ]))
        );
    }

    #[test]
    fn a_sub_directory_is_nested_in_the_deepest_enclosing_role_and_equal_cwds_are_not() {
        let m = nesting(&[
            pair("tj", "C:\\Projects\\ThinkersJournal-Community"),
            pair("api", "C:/Projects/thinkersjournal-community/apps/api/"),
            pair("apps", "C:/Projects/ThinkersJournal-Community/apps"),
            pair("twin", "C:/Projects/ThinkersJournal-Community"),
            pair("other", "C:/Projects/ThinkersJournal-Community2"),
        ]);
        assert_eq!(m.get("api").map(String::as_str), Some("apps"), "{m:?}");
        assert_eq!(m.get("apps").map(String::as_str), Some("tj"), "{m:?}");
        // same directory: not nesting; and a sibling prefix without a `/` boundary is not either
        assert!(!m.contains_key("tj") && !m.contains_key("twin"), "{m:?}");
        assert!(!m.contains_key("other"), "{m:?}");
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

    fn lane_state_with_args(dir: &Path, role: &str, args: Option<Vec<String>>) {
        let mut st = crate::testing::state_at(role, "s", "Stop");
        st.pid = 55;
        st.pid_start_secs = Some(100);
        st.launch_args = args;
        std::fs::write(
            dir.join(format!("{role}.json")),
            serde_json::to_string(&st).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn a_live_role_shows_which_push_channels_it_was_launched_with() {
        let args = |a: &[&str]| Some(a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let ch = |role: &str, a: Option<Vec<String>>, facts: &RecordingFacts| {
            let d = tempdir().unwrap();
            lane_state_with_args(d.path(), role, a);
            role_status(d.path(), role, facts)
                .channels
                .map(|c| c.text())
        };
        let live = facts_with_live(55, 100);
        assert_eq!(
            ch(
                "a",
                args(&[
                    "--mcp-config",
                    "m",
                    "--dangerously-load-development-channels",
                    "server:claude-peers",
                    "server:synapse"
                ]),
                &live
            )
            .as_deref(),
            Some("mcp+peers+synapse")
        );
        // control: a different launch reads differently
        assert_eq!(
            ch(
                "b",
                args(&[
                    "--dangerously-load-development-channels",
                    "server:claude-peers"
                ]),
                &live
            )
            .as_deref(),
            Some("-+peers+-")
        );
        // recorded nothing: no verdict (the column prints `?`); not live: no column
        assert_eq!(ch("c", None, &live), None);
        assert_eq!(ch("d", args(&["--mcp-config", "m"]), &dead_facts()), None);
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
    fn quiet_over_marks_only_when_asked_and_only_roles_older_than_the_threshold() {
        let d = tempdir().unwrap();
        let facts = facts_with_live(55, 100);
        let mut st = crate::testing::state_at("baracuda", "s", "Stop");
        st.pid = 55;
        st.pid_start_secs = Some(100);
        // the fake clock is fixed, so the write is placed relative to it, not to the wall clock
        st.updated_at = facts.now() - chrono::Duration::seconds(7200);
        std::fs::write(
            d.path().join("baracuda.json"),
            serde_json::to_string(&st).unwrap(),
        )
        .unwrap();
        let mut r = status_all(d.path(), &facts);
        let a = r.roles[0].activity.as_ref().unwrap();
        assert_eq!((a.quiet_over, r.quiet_over_secs), (None, None));
        assert!(!serde_json::to_string(&r).unwrap().contains("quiet_over"));
        r.mark_quiet_over(3600);
        assert_eq!(r.roles[0].activity.as_ref().unwrap().quiet_over, Some(true));
        r.mark_quiet_over(86_400);
        assert_eq!(
            r.roles[0].activity.as_ref().unwrap().quiet_over,
            Some(false)
        );
        assert_eq!(r.quiet_over_secs, Some(86_400));
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

    fn peer_row(id: &str, pid: u32) -> crate::notify::Peer {
        crate::notify::Peer {
            id: id.into(),
            cwd: "C:/x".into(),
            pid,
            summary: Some("hi".into()),
            last_seen: None,
        }
    }

    #[test]
    fn peers_join_a_live_role_by_pid_and_leave_a_dead_roles_session_unmatched() {
        let d = tempdir().unwrap();
        lane_state_file(d.path(), "live", 55, Some(100));
        lane_state_file(d.path(), "dead", 66, Some(100));
        let facts = facts_with_live(55, 100);
        let mut r = status_all(d.path(), &facts);
        // Without the flag nothing about peers is in the JSON.
        let plain = serde_json::to_string(&r).unwrap();
        assert!(!plain.contains("peers"), "{plain}");
        crate::peers_join::annotate(
            &mut r,
            d.path(),
            &facts,
            &|pid| match pid {
                11 => Some(55),
                22 => Some(66),
                _ => None,
            },
            Ok(vec![peer_row("p1", 11), peer_row("p2", 22)]),
        );
        let by: std::collections::BTreeMap<_, _> =
            r.roles.iter().map(|s| (s.role.as_str(), s)).collect();
        match by["live"].peers.as_ref().unwrap() {
            crate::peers_join::RolePeers::Peers(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].id, "p1");
            }
            other => panic!("{other:?}"),
        }
        match by["dead"].peers.as_ref().unwrap() {
            crate::peers_join::RolePeers::Peers(v) => assert!(v.is_empty()),
            other => panic!("{other:?}"),
        }
        let un = r.peers_unmatched.as_ref().unwrap();
        assert_eq!(un.len(), 1);
        assert_eq!(un[0].id, "p2");
        assert!(r.peers_error.is_none());
    }

    #[test]
    fn an_unreadable_broker_is_unknown_for_every_role_never_no_peer() {
        let d = tempdir().unwrap();
        lane_state_file(d.path(), "live", 55, Some(100));
        let facts = facts_with_live(55, 100);
        let mut r = status_all(d.path(), &facts);
        crate::peers_join::annotate(&mut r, d.path(), &facts, &|_| None, Err("down".into()));
        assert_eq!(
            r.roles[0].peers,
            Some(crate::peers_join::RolePeers::unknown())
        );
        assert_eq!(r.peers_error.as_deref(), Some("down"));
        assert!(r.peers_unmatched.is_none());
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains(r#""peers":"unknown""#), "{json}");
    }

    // ---- launch mode `wsl` (docs/LANE-RESTART-WSL.md) ----

    fn pilot_target() -> WslTarget {
        WslTarget {
            distro: "lane-spike".to_string(),
            user: "lane-commysdk".to_string(),
            wrapper: "/usr/local/bin/lane-sandbox".to_string(),
            state_dir: None,
        }
    }

    fn wsl_spec(role: &str) -> SpecFile {
        let mut l = launch(role);
        l.cwd = "/home/lane-commysdk/work/commy-sdk-rust".to_string();
        l.launch_args = Some(
            [
                "claude",
                "--mcp-config",
                "/home/lane-commysdk/synapse-home/lane.mcp.json",
                "--dangerously-load-development-channels",
                "server:synapse",
            ]
            .map(String::from)
            .to_vec(),
        );
        build(&inputs(&l, "sonnet", SpecState::Running))
            .unwrap()
            .with_wsl(pilot_target())
    }

    #[test]
    fn a_windows_spec_has_exactly_todays_keys_and_schema_1() {
        let v = serde_json::to_value(built("agentlife", SpecState::Running)).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "cwd",
                "handoff_mtime",
                "handoff_path",
                "launch_args",
                "model",
                "name",
                "permission_mode",
                "remote_control",
                "role",
                "schema",
                "state",
                "stop_kind",
                "stop_reason",
                "stopped_pid_start_secs",
                "stopped_session_id",
                "tool_version",
                "written_at",
                "written_by",
            ]
        );
        assert_eq!(v["schema"], 1);
    }

    #[test]
    fn a_wsl_spec_is_schema_2_and_round_trips_through_the_atomic_write() {
        let d = tempdir().unwrap();
        let s = wsl_spec("commy-sdk-rust");
        write_atomic(d.path(), &s).unwrap();
        assert_eq!(read(d.path(), "commy-sdk-rust").unwrap(), s);
        let on_disk: serde_json::Value = serde_json::from_slice(
            &std::fs::read(d.path().join("commy-sdk-rust.launch.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(on_disk["schema"], 2);
        assert_eq!(on_disk["launch"], "wsl");
        assert_eq!(on_disk["wsl"]["distro"], "lane-spike");
        assert_eq!(on_disk["wsl"]["user"], "lane-commysdk");
        assert_eq!(on_disk["wsl"]["wrapper"], "/usr/local/bin/lane-sandbox");
        assert!(on_disk["wsl"].get("state_dir").is_none());
    }

    #[test]
    fn a_build_that_reads_only_schema_1_would_refuse_a_wsl_spec() {
        // The mechanism the design relies on: a Windows lane-restart of today compares the schema
        // with 1 and answers `UnsupportedSchema`, so a WSL spec, written as 2, is never launched
        // by it as a Windows process with a POSIX cwd.
        let v = serde_json::to_value(wsl_spec("commy-sdk-rust")).unwrap();
        assert_ne!(v["schema"].as_u64().unwrap(), 1);
        assert_eq!(SCHEMA, 1, "Windows specs keep schema 1");
    }

    #[test]
    fn launch_fields_and_schema_must_agree_or_the_spec_is_refused() {
        let ok = wsl_spec("commy-sdk-rust");
        assert!(ok.check().is_ok(), "{:?}", ok.check());
        // schema 1 carrying the wsl fields
        let mut s = ok.clone();
        s.schema = 1;
        assert!(
            matches!(s.check(), Err(SpecError::Incomplete(_))),
            "{:?}",
            s.check()
        );
        // schema 2 without a launch mode
        let mut s = ok.clone();
        s.launch = None;
        assert!(
            matches!(s.check(), Err(SpecError::Incomplete(_))),
            "{:?}",
            s.check()
        );
        // schema 2, launch wsl, no target
        let mut s = ok.clone();
        s.wsl = None;
        assert!(
            matches!(s.check(), Err(SpecError::Incomplete(_))),
            "{:?}",
            s.check()
        );
        // a launch mode this build does not know is never read as Windows
        let mut s = ok.clone();
        s.launch = Some("lxc".to_string());
        assert_eq!(
            s.check().unwrap_err(),
            SpecError::UnsupportedLaunch("lxc".to_string())
        );
        // a schema this build does not know
        let mut s = ok.clone();
        s.schema = 3;
        assert_eq!(s.check().unwrap_err(), SpecError::UnsupportedSchema(3));
        // a Windows spec with a stray wsl object
        let mut s = built("agentlife", SpecState::Running);
        s.wsl = Some(pilot_target());
        assert!(
            matches!(s.check(), Err(SpecError::Incomplete(_))),
            "{:?}",
            s.check()
        );
    }

    #[test]
    fn a_wsl_spec_with_a_bad_field_or_a_windows_cwd_is_refused_at_write() {
        let d = tempdir().unwrap();
        let mut s = wsl_spec("commy-sdk-rust");
        s.wsl.as_mut().unwrap().user = "root".to_string();
        assert!(matches!(
            write_atomic(d.path(), &s),
            Err(SpecError::Incomplete(_))
        ));
        let mut s = wsl_spec("commy-sdk-rust");
        s.cwd = "C:/Projects/commy-sdk-rust".to_string();
        assert!(matches!(
            write_atomic(d.path(), &s),
            Err(SpecError::Incomplete(_))
        ));
        assert_eq!(
            std::fs::read_dir(d.path()).unwrap().count(),
            0,
            "nothing written"
        );
    }

    #[test]
    fn a_hand_edited_wsl_spec_is_refused_at_read_too() {
        let d = tempdir().unwrap();
        write_atomic(d.path(), &wsl_spec("commy-sdk-rust")).unwrap();
        let path = d.path().join("commy-sdk-rust.launch.json");
        let mut v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        v["wsl"]["wrapper"] = "/usr/bin/curl;evil".into();
        std::fs::write(&path, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
        assert!(read(d.path(), "commy-sdk-rust").is_err());
        v["wsl"]["wrapper"] = "/usr/local/bin/lane-sandbox".into();
        v["launch"] = "lxc".into();
        std::fs::write(&path, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
        assert_eq!(
            read(d.path(), "commy-sdk-rust").unwrap_err(),
            SpecError::UnsupportedLaunch("lxc".to_string())
        );
    }

    #[test]
    fn the_wsl_argv_is_the_pilot_command_with_prompt_first_and_the_variadic_flag_last() {
        let argv = wsl_spec("commy-sdk-rust").wsl_argv().unwrap();
        assert_eq!(
            argv[..10],
            [
                "wsl.exe",
                "-d",
                "lane-spike",
                "-u",
                "lane-commysdk",
                "--cd",
                "/home/lane-commysdk/work/commy-sdk-rust",
                "--exec",
                "/usr/local/bin/lane-sandbox",
                "--",
            ]
        );
        assert_eq!(argv[10], "claude");
        assert_eq!(argv[11], crate::launch::startup_prompt("commy-sdk-rust"));
        let tail = &argv[12..];
        let pos = |x: &str| tail.iter().position(|a| a == x).unwrap();
        assert_eq!(tail[pos("--model") + 1], "sonnet");
        assert_eq!(tail[pos("--name") + 1], "commy-sdk-rust");
        assert_eq!(
            tail[pos("--mcp-config") + 1],
            "/home/lane-commysdk/synapse-home/lane.mcp.json"
        );
        assert_eq!(
            argv[argv.len() - 2],
            "--dangerously-load-development-channels"
        );
        assert_eq!(argv[argv.len() - 1], "server:synapse");
    }

    #[test]
    fn an_opus_wsl_spec_is_refused_like_any_other() {
        let d = tempdir().unwrap();
        let mut s = wsl_spec("commy-sdk-rust");
        s.model = "claude-opus-5-5".to_string();
        assert!(matches!(
            write_atomic(d.path(), &s),
            Err(SpecError::Incomplete(_))
        ));
        assert!(s.wsl_argv().is_err());
    }

    #[test]
    fn a_windows_spec_has_no_wsl_argv() {
        assert!(built("agentlife", SpecState::Running).wsl_argv().is_err());
    }

    #[test]
    fn the_launch_spec_of_a_wsl_spec_carries_the_target_and_a_windows_spec_carries_none() {
        let w = wsl_spec("commy-sdk-rust");
        assert_eq!(w.launch_spec().wsl, Some(pilot_target()));
        assert_eq!(
            w.launch_spec().cwd,
            "/home/lane-commysdk/work/commy-sdk-rust"
        );
        assert_eq!(
            built("agentlife", SpecState::Running).launch_spec().wsl,
            None
        );
        // and the command the spec reports is the one the host would run
        let prepared = w.check().unwrap();
        assert_eq!(
            prepared.argv.wsl_command().map(<[String]>::to_vec),
            Some(w.wsl_argv().unwrap())
        );
    }
}
