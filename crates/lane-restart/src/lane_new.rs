// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart lane-new <role> <repo>` - docs/LANE-NEW.md. **Slice 1: the local-file steps only.**
//! Step 0 (validate), 3 (MCP config), 4 (launch spec) and 5 (soak list); the repo, seed, provisioning
//! and start steps are printed in the plan as later slices and never run here.
//!
//! The rules that are the point of the module (the PM's three paid failure modes, 2026-10-30):
//! * **No hand-written JSON.** The spec is a `SpecFile` written by `spec::write_atomic`, the MCP config a
//!   `serde_json::Value`: a backslash in a path is escaped by the serializer, never by this file.
//! * **No relative path.** Every path comes from [`Roots`], whose constructor refuses a relative one, so
//!   the process's cwd is never consulted.
//! * **A never-started lane is `suspended` with no `stop_kind`.** `lane-restart start` accepts it (the
//!   PM started six that way); the launcher service refuses it (`stop-unclassified`), so a new lane
//!   does not wake on mail before the PM starts it.
//!
//! Every step is check, act, read back; a step whose end state already holds is `already` and does
//! nothing, so rerunning converges. Nothing is overwritten: an existing artifact that differs from what
//! this would write is a failure, found by step 0 before any step writes.

use crate::lane_new_host::{host_log_since, Host};
use crate::lane_new_remote::{self as remote, Vcs};
use crate::spec::{self, SpecFile, SpecState, WrittenBy};
use crate::start;
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};

/// The org every lane repo lives in (PM answer 2026-10-30: `ThinkersJournal/rockfish` exists).
pub const ORG: &str = "ThinkersJournal";
const SOAK_FILE: &str = "roles-soak.txt";
const SYNAPSE_ADDR: &str = "127.0.0.1:7920";

/// The three roots every path derives from, plus the two Synapse constants. All absolute.
#[derive(Debug, Clone)]
pub struct Roots {
    pub state_dir: PathBuf,
    pub projects: PathBuf,
    pub lane_homes: PathBuf,
    pub pilot_home: PathBuf,
    pub channel_exe: PathBuf,
}

impl Roots {
    /// `Err` naming the first path that is not absolute: a relative root is how a changed cwd breaks a
    /// run (the PM's `> .lane-state/x.log` incident).
    pub fn new(
        state_dir: PathBuf,
        projects: PathBuf,
        lane_homes: PathBuf,
        pilot_home: PathBuf,
        channel_exe: PathBuf,
    ) -> Result<Roots, String> {
        for (what, p) in [
            ("state dir", &state_dir),
            ("projects root", &projects),
            ("lane-home root", &lane_homes),
            ("pilot home", &pilot_home),
            ("channel exe", &channel_exe),
        ] {
            if !p.is_absolute() {
                return Err(format!(
                    "the {what} is not an absolute path: {}",
                    p.display()
                ));
            }
        }
        Ok(Roots {
            state_dir,
            projects,
            lane_homes,
            pilot_home,
            channel_exe,
        })
    }

    /// The layout on this machine: the projects root is the state dir's parent; the Synapse homes are
    /// under `%LOCALAPPDATA%` and the channel binary in `%USERPROFILE%\.cargo\bin`.
    pub fn standard(state_dir: PathBuf) -> Result<Roots, String> {
        let projects = state_dir
            .parent()
            .ok_or("the state dir has no parent")?
            .to_path_buf();
        let local = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is not set")?;
        let profile = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .ok_or("USERPROFILE is not set")?;
        let local = PathBuf::from(local);
        let exe = if cfg!(windows) {
            "synapse-claude-channel.exe"
        } else {
            "synapse-claude-channel"
        };
        Roots::new(
            state_dir,
            projects,
            local.join("synapse-lanes"),
            local.join("synapse-pilot"),
            PathBuf::from(profile).join(".cargo").join("bin").join(exe),
        )
    }

    fn mcp_path(&self, role: &str) -> PathBuf {
        self.state_dir.join(format!("{role}.mcp.json"))
    }
    fn spec_path(&self, role: &str) -> PathBuf {
        self.state_dir.join(format!("{role}.launch.json"))
    }
    fn lane_home(&self, role: &str) -> PathBuf {
        self.lane_homes.join(role)
    }
    fn repo_dir(&self, repo: &str) -> PathBuf {
        self.projects.join(repo)
    }
    fn soak_path(&self) -> PathBuf {
        self.state_dir.join(SOAK_FILE)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Acted, and the read-back confirmed it.
    Ok,
    /// The end state already held; nothing was done.
    Already,
    /// Dry run: this would be done.
    Would,
    /// Not done, or the read-back did not confirm it. The run stops here.
    Fail,
    /// Not part of this build (a later slice).
    Later,
}

#[derive(Debug, Clone)]
pub struct StepRow {
    pub n: u8,
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
}

#[derive(Debug)]
pub struct Report {
    pub rows: Vec<StepRow>,
    /// Every path the report names (all absolute; tested).
    pub paths: Vec<PathBuf>,
    /// Set by a dry run that found nothing wrong: [`plan_hash`] of the arguments it planned. `--apply` must
    /// be given this value.
    pub plan: Option<String>,
}

impl Report {
    pub fn failed(&self) -> bool {
        self.rows.iter().any(|r| r.status == Status::Fail)
    }

    pub fn render(&self, apply: bool) -> String {
        let mut s = format!(
            "lane-new: {}\n",
            if apply {
                "APPLY"
            } else {
                "DRY RUN (nothing is written or run)"
            }
        );
        for r in &self.rows {
            let tag = match r.status {
                Status::Ok => "ok",
                Status::Already => "already",
                Status::Would => "would",
                Status::Fail => "FAIL",
                Status::Later => "later",
            };
            s.push_str(&format!(
                "  step {} {:<22} {:<7} {}\n",
                r.n, r.name, tag, r.detail
            ));
        }
        if let Some(p) = &self.plan {
            s.push_str(&format!(
                "plan {p}\n  to act, repeat the same role, repo and model with: --apply --plan {p}\n"
            ));
        }
        s
    }
}

/// Fingerprint of what a run would do, from the arguments alone (never from what exists, so a resume after a
/// failed step still matches the plan that was read): role, org/repo, model, the roots every path derives
/// from, and the name and SHA-256 of every seed file (which carries the repo name into the stub crate). A
/// typo in the role or repo changes it, and so does a new build with a different seed. Sixteen hex digits.
pub fn plan_hash(roots: &Roots, role: &str, repo: &str, model: &str) -> Result<String, String> {
    let seed = remote::render(ORG, role, repo)?;
    let mut s = format!(
        "lane-new plan v1\nrole={role}\nrepo={ORG}/{repo}\nmodel={model}\nstate_dir={}\nprojects={}\nlane_homes={}\npilot_home={}\nchannel_exe={}\n",
        roots.state_dir.display(),
        roots.projects.display(),
        roots.lane_homes.display(),
        roots.pilot_home.display(),
        roots.channel_exe.display()
    );
    for f in &seed {
        s.push_str(&format!(
            "seed {} {}\n",
            f.path,
            remote::sha256_hex(&f.bytes)
        ));
    }
    Ok(remote::sha256_hex(s.as_bytes())[..16].to_string())
}

/// `--apply`: refuses unless `plan` is the [`plan_hash`] of exactly these arguments, then runs for real. The
/// only way the binary reaches `run_full` with `apply` true. Nothing is read, written or run on a mismatch.
#[allow(clippy::too_many_arguments)]
pub fn run_apply(
    roots: &Roots,
    vcs: Option<&dyn Vcs>,
    host: Option<&dyn Host>,
    role: &str,
    repo: &str,
    model: &str,
    plan: Option<&str>,
    now: DateTime<Utc>,
) -> Report {
    let refuse = |why: String| Report {
        rows: vec![StepRow {
            n: 0,
            name: "plan",
            status: Status::Fail,
            detail: why,
        }],
        paths: Vec::new(),
        plan: None,
    };
    let Some(given) = plan else {
        return refuse("--apply needs --plan <hash>, printed at the end of a dry run of the same role, repo and model".to_string());
    };
    match plan_hash(roots, role, repo, model) {
        Ok(want) if want == given => run_full(roots, vcs, host, role, repo, model, true, now),
        Ok(_) => refuse(format!(
            "--plan {given} is not the plan for role {role:?}, repo {repo:?}, model {model:?}: dry-run these exact arguments again and use the hash it prints"
        )),
        Err(e) => refuse(format!("cannot plan these arguments: {e}")),
    }
}

/// Role and repo names: the weekly provisioning script's rule, so a name this accepts that script accepts.
pub fn safe_name(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
        && !s.contains("..")
}

/// The MCP config a lane gets, as a value (the serializer escapes every path).
pub fn mcp_config(roots: &Roots, role: &str) -> serde_json::Value {
    let home = fwd(&roots.lane_home(role));
    serde_json::json!({
        "mcpServers": { "synapse": {
            "command": fwd(&roots.channel_exe),
            "args": ["--role", role, "--home", home],
            "env": {
                "SYNAPSE_HOME": home,
                "SYNAPSE_ADDR": SYNAPSE_ADDR,
                "SYNAPSE_DAEMON_HOME": fwd(&roots.pilot_home),
            },
        }}
    })
}

/// Forward slashes, as the existing lanes' MCP configs write them.
fn fwd(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// The spec a new lane gets. `suspended`, no `stop_kind`, never `running`.
pub fn build_spec(
    roots: &Roots,
    role: &str,
    repo: &str,
    model: &str,
    now: DateTime<Utc>,
) -> Result<SpecFile, String> {
    let r = lane_state::role::Role::new(role).map_err(|e| format!("role {role:?}: {e}"))?;
    let cwd = roots.repo_dir(repo);
    Ok(SpecFile {
        schema: spec::SCHEMA,
        role: r.clone(),
        name: r,
        cwd: cwd.to_string_lossy().into_owned(),
        model: model.to_string(),
        permission_mode: Some(start::START_PERMISSION_MODE),
        remote_control: false,
        launch_args: vec![
            "--mcp-config".into(),
            roots.mcp_path(role).to_string_lossy().into_owned(),
            "--dangerously-load-development-channels".into(),
            "server:claude-peers".into(),
            "server:synapse".into(),
        ],
        handoff_path: Some(cwd.join("HANDOFF.md").to_string_lossy().into_owned()),
        handoff_mtime: None,
        state: SpecState::Suspended,
        written_at: now,
        written_by: WrittenBy::Other("lane-new".into()),
        stopped_session_id: None,
        stopped_pid_start_secs: None,
        stop_reason: None,
        stop_kind: None,
        stop_mac: None,
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        launch: None,
        wsl: None,
    })
}

/// Same lane? Everything a launch reads; not the timestamps or the tool version.
fn same_launch(a: &SpecFile, b: &SpecFile) -> bool {
    a.role == b.role
        && a.name == b.name
        && a.cwd == b.cwd
        && a.model == b.model
        && a.permission_mode == b.permission_mode
        && a.remote_control == b.remote_control
        && a.launch_args == b.launch_args
        && a.handoff_path == b.handoff_path
        && a.state == b.state
        && a.stop_kind == b.stop_kind
        && a.launch == b.launch
        && a.wsl == b.wsl
}

/// The role names a soak file lists, by the weekly script's rule. `Err` on an unsafe line, which the
/// script would refuse the whole file for.
fn soak_roles(text: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if !safe_name(t) {
            return Err("the roles file holds an unsafe role name".to_string());
        }
        out.push(t.to_string());
    }
    Ok(out)
}

/// `text` with `role` added as a line, keeping the file's own line ending.
fn soak_with(text: &str, role: &str) -> String {
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut s = text.to_string();
    if !s.is_empty() && !s.ends_with('\n') {
        s.push_str(nl);
    }
    s.push_str(role);
    s.push_str(nl);
    s
}

fn write_text_atomic(path: &Path, text: &str) -> Result<(), String> {
    let dir = path.parent().ok_or("no parent directory")?;
    let tmp = dir.join(format!(
        ".{}.lane-new.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file")
    ));
    let r = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, path));
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r.map_err(|e| format!("cannot write {}: {e}", path.display()))
}

struct Want {
    spec: SpecFile,
    mcp: String,
}

fn fail(rows: &mut Vec<StepRow>, n: u8, name: &'static str, detail: String) {
    rows.push(StepRow {
        n,
        name,
        status: Status::Fail,
        detail,
    });
}

/// Step 6: the Synapse identity. `already` if the lane home holds the certificate and the announce file;
/// else (with `apply`) runs the provisioning script for this role alone and reads back both its counts
/// and the files.
fn step_identity(
    host: &dyn Host,
    roots: &Roots,
    role: &str,
    apply: bool,
) -> Result<(Status, String), String> {
    let home = roots.lane_home(role);
    let cert = home.join("roles").join(role).join("cert.pem");
    let announce = home.join("synapsed.json");
    let files = || (cert.is_file(), announce.is_file());
    if files() == (true, true) {
        return Ok((Status::Already, home.display().to_string()));
    }
    if !apply {
        return Ok((
            Status::Would,
            format!(
                "synapse-provision-weekly.ps1 -Apply for {role} alone; home {}",
                home.display()
            ),
        ));
    }
    let roles_file = roots.state_dir.join(format!(".lane-new-{role}.roles.tmp"));
    write_text_atomic(&roles_file, &format!("{role}\n"))?;
    let counts = host.provision(&roles_file);
    let _ = std::fs::remove_file(&roles_file);
    let c = counts?;
    if c.refused != 0 || c.other != 0 || c.total() != 1 || c.created + c.ok + c.renewed != 1 {
        return Err(format!(
            "the provisioning script reported ok={} created={} renewed={} refused={} other={}; expected exactly one role, none refused",
            c.ok, c.created, c.renewed, c.refused, c.other
        ));
    }
    match files() {
        (true, true) => Ok((
            Status::Ok,
            format!(
                "created={} ok={}; certificate and announce file in {}",
                c.created,
                c.ok,
                home.display()
            ),
        )),
        (has_cert, has_announce) => Err(format!(
            "the script reported success but {} lacks {}",
            home.display(),
            match (has_cert, has_announce) {
                (false, false) => "the certificate and synapsed.json",
                (false, true) => "the certificate",
                _ => "synapsed.json",
            }
        )),
    }
}

/// Step 7: start the lane. The spec is `suspended` until this; the read-back is the lane's own state file
/// naming a live `claude`, the spec now `running`, and a non-empty host log written since this step began.
fn step_start(
    host: &dyn Host,
    roots: &Roots,
    role: &str,
    model: &str,
    apply: bool,
) -> Result<(Status, String), String> {
    let cmd = format!("lane-restart start --role {role} --model {model} --reason lane-new");
    if !apply {
        return Ok((Status::Would, cmd));
    }
    let since = std::time::SystemTime::now();
    let code = host.start(role, model)?;
    let state = crate::state::load(&roots.state_dir, role)
        .map_err(|e| format!("start returned {code} but the lane wrote no state: {e}"))?;
    if !host.claude_alive(state.pid) {
        return Err(format!(
            "start returned {code} but pid {} in {role}.json is not a live claude",
            state.pid
        ));
    }
    let have = spec::read(&roots.state_dir, role).map_err(|e| e.to_string())?;
    if have.state != SpecState::Running {
        return Err(format!(
            "start returned {code} but the spec is {:?}, not running",
            have.state
        ));
    }
    let log = host_log_since(&roots.state_dir, role, since).ok_or_else(|| {
        format!(
            "start returned {code} but there is no non-empty host-{role}-<pid>.log in {} written since the start",
            roots.state_dir.display()
        )
    })?;
    let dialog = if code == 2 {
        "; AWAITING the dev-channels dialog: someone must confirm it"
    } else {
        ""
    };
    Ok((
        Status::Ok,
        format!(
            "pid {} live, spec running, host log {}{dialog}",
            state.pid,
            log.display()
        ),
    ))
}

/// Slice 1: steps 0, 3, 4 and 5 with no `gh`/`git`; steps 1 and 2 are listed as later.
pub fn run_local(
    roots: &Roots,
    role: &str,
    repo: &str,
    model: &str,
    apply: bool,
    now: DateTime<Utc>,
) -> Report {
    run_with(roots, None, role, repo, model, apply, now)
}

/// Runs steps 0, 1, 2, 3, 4 and 5 (writing and creating only if `apply`; a dry run only reads), then lists
/// the later steps. Step 0 reads every existing artifact first, so a conflict is found before anything is
/// written. With `vcs` `None`, steps 1 and 2 are listed as later (slice 1).
pub fn run_with(
    roots: &Roots,
    vcs: Option<&dyn Vcs>,
    role: &str,
    repo: &str,
    model: &str,
    apply: bool,
    now: DateTime<Utc>,
) -> Report {
    run_full(roots, vcs, None, role, repo, model, apply, now)
}

/// Slice 3: as [`run_with`], plus the disk floor in step 0 and steps 6 (Synapse identity) and 7 (start)
/// through `host`. With `host` `None` those two steps are listed as later.
#[allow(clippy::too_many_arguments)]
pub fn run_full(
    roots: &Roots,
    vcs: Option<&dyn Vcs>,
    host: Option<&dyn Host>,
    role: &str,
    repo: &str,
    model: &str,
    apply: bool,
    now: DateTime<Utc>,
) -> Report {
    let nwo = format!("{ORG}/{repo}");
    let mut rows = Vec::new();
    let mut paths = vec![
        roots.state_dir.clone(),
        roots.repo_dir(repo),
        roots.mcp_path(role),
        roots.spec_path(role),
        roots.soak_path(),
        roots.lane_home(role),
    ];
    paths.dedup();
    let rep = |rows: Vec<StepRow>, paths: Vec<PathBuf>| Report {
        rows,
        paths,
        plan: None,
    };

    // --- step 0: validate, and read what exists -----------------------------------------------
    let mut problems = Vec::new();
    if !safe_name(role) || !lane_state::role::is_valid(role) {
        problems.push(format!("role {role:?} is not a plain identifier"));
    }
    if role == start::PM_ROLE {
        problems.push("the role pm cannot be created".to_string());
    }
    if !safe_name(repo) {
        problems.push(format!("repo {repo:?} is not a plain repository name"));
    }
    let (pinned, replaced) = spec::pin_model(model);
    if replaced.is_some() || pinned != model {
        problems.push(format!(
            "model {model:?} is not sonnet or haiku (never Opus)"
        ));
    }
    if !problems.is_empty() {
        fail(&mut rows, 0, "validate", problems.join("; "));
        return rep(rows, paths);
    }
    let want = match build_spec(roots, role, repo, model, now) {
        Ok(spec) => Want {
            spec,
            mcp: serde_json::to_string_pretty(&mcp_config(roots, role)).unwrap_or_default() + "\n",
        },
        Err(e) => {
            fail(&mut rows, 0, "validate", e);
            return rep(rows, paths);
        }
    };
    if let Err(e) = want.spec.check() {
        fail(
            &mut rows,
            0,
            "validate",
            format!("the spec would be incomplete: {e}"),
        );
        return rep(rows, paths);
    }
    if roots.state_dir.join(format!("{role}.json")).exists() {
        problems.push(format!("{role}.json exists: a lane with this role has run"));
    }
    let mcp_have = std::fs::read_to_string(roots.mcp_path(role)).ok();
    let mcp_same = match &mcp_have {
        Some(t) => {
            serde_json::from_str::<serde_json::Value>(t).ok() == Some(mcp_config(roots, role))
        }
        None => false,
    };
    if mcp_have.is_some() && !mcp_same {
        problems.push(format!(
            "{} exists and differs",
            roots.mcp_path(role).display()
        ));
    }
    let spec_same = match spec::read(&roots.state_dir, role) {
        Ok(have) => {
            if !same_launch(&have, &want.spec) {
                problems.push(format!(
                    "{} exists and differs",
                    roots.spec_path(role).display()
                ));
            }
            true
        }
        Err(spec::SpecError::NotFound(_)) => false,
        Err(e) => {
            problems.push(format!(
                "{} is unusable: {e}",
                roots.spec_path(role).display()
            ));
            false
        }
    };
    let soak_text = std::fs::read_to_string(roots.soak_path());
    let soak_have = match &soak_text {
        Ok(t) => match soak_roles(t) {
            Ok(v) => Some(v),
            Err(e) => {
                problems.push(e);
                None
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            problems.push(format!("{} not found", roots.soak_path().display()));
            None
        }
        Err(e) => {
            problems.push(format!("cannot read {}: {e}", roots.soak_path().display()));
            None
        }
    };
    let seed = if vcs.is_some() {
        match remote::render(ORG, role, repo) {
            Ok(s) => Some(s),
            Err(e) => {
                problems.push(e);
                None
            }
        }
    } else {
        None
    };
    if let Some(v) = vcs {
        problems.extend(remote::local_problems(v, &roots.repo_dir(repo), &nwo));
    }
    if let Some(h) = host {
        if let Err(why) = h.disk_floor(&roots.state_dir) {
            problems.push(format!("disk floor: {why}"));
        }
    }
    if !problems.is_empty() {
        fail(&mut rows, 0, "validate", problems.join("; "));
        return rep(rows, paths);
    }
    rows.push(StepRow {
        n: 0,
        name: "validate",
        status: Status::Ok,
        detail: format!("{role} / {nwo} / {model}; nothing in the way"),
    });

    // --- steps 1 and 2: the repo, then the clone and seed (slice 2) ----------------------------
    if let (Some(v), Some(seed)) = (vcs, seed.as_ref()) {
        let status = |o: remote::Outcome| match o {
            remote::Outcome::Ok => Status::Ok,
            remote::Outcome::Already => Status::Already,
            remote::Outcome::Would => Status::Would,
        };
        let repo_made = match remote::step_repo(v, &nwo, apply) {
            Ok((o, detail)) => {
                rows.push(StepRow {
                    n: 1,
                    name: "create repo",
                    status: status(o),
                    detail,
                });
                o != remote::Outcome::Would
            }
            Err(e) => {
                fail(&mut rows, 1, "create repo", e);
                return rep(rows, paths);
            }
        };
        match remote::step_seed(v, &nwo, &roots.repo_dir(repo), seed, apply, repo_made) {
            Ok((o, detail)) => rows.push(StepRow {
                n: 2,
                name: "clone + seed + CI",
                status: status(o),
                detail,
            }),
            Err(e) => {
                fail(&mut rows, 2, "clone + seed + CI", e);
                return rep(rows, paths);
            }
        }
    }

    // --- step 3: MCP config --------------------------------------------------------------------
    let p = roots.mcp_path(role);
    if mcp_same {
        rows.push(StepRow {
            n: 3,
            name: "mcp config",
            status: Status::Already,
            detail: p.display().to_string(),
        });
    } else if !apply {
        rows.push(StepRow {
            n: 3,
            name: "mcp config",
            status: Status::Would,
            detail: format!("write {}", p.display()),
        });
    } else {
        let back = write_text_atomic(&p, &want.mcp).and_then(|()| {
            let t = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
            let v: serde_json::Value = serde_json::from_str(&t).map_err(|e| e.to_string())?;
            if v == mcp_config(roots, role) {
                Ok(())
            } else {
                Err("the file read back differs from what was written".to_string())
            }
        });
        match back {
            Ok(()) => rows.push(StepRow {
                n: 3,
                name: "mcp config",
                status: Status::Ok,
                detail: format!("{} read back equal", p.display()),
            }),
            Err(e) => {
                fail(&mut rows, 3, "mcp config", e);
                return rep(rows, paths);
            }
        }
    }

    // --- step 4: launch spec -------------------------------------------------------------------
    let p = roots.spec_path(role);
    if spec_same {
        rows.push(StepRow {
            n: 4,
            name: "launch spec",
            status: Status::Already,
            detail: p.display().to_string(),
        });
    } else if !apply {
        rows.push(StepRow {
            n: 4,
            name: "launch spec",
            status: Status::Would,
            detail: format!("write {} (suspended, no stop_kind)", p.display()),
        });
    } else {
        let back = spec::write_atomic(&roots.state_dir, &want.spec)
            .map_err(|e| e.to_string())
            .and_then(|()| spec::read(&roots.state_dir, role).map_err(|e| e.to_string()))
            .and_then(|have| {
                if same_launch(&have, &want.spec) {
                    Ok(())
                } else {
                    Err("the spec read back differs from what was written".to_string())
                }
            });
        match back {
            Ok(()) => rows.push(StepRow {
                n: 4,
                name: "launch spec",
                status: Status::Ok,
                detail: format!("{} read back equal", p.display()),
            }),
            Err(e) => {
                fail(&mut rows, 4, "launch spec", e);
                return rep(rows, paths);
            }
        }
    }

    // --- step 5: soak list ---------------------------------------------------------------------
    let p = roots.soak_path();
    let have = soak_have.unwrap_or_default();
    let n_have = have.iter().filter(|r| r.as_str() == role).count();
    if n_have == 1 {
        rows.push(StepRow {
            n: 5,
            name: "soak list",
            status: Status::Already,
            detail: p.display().to_string(),
        });
    } else if n_have > 1 {
        fail(
            &mut rows,
            5,
            "soak list",
            format!("{role} is listed {n_have} times"),
        );
        return rep(rows, paths);
    } else if !apply {
        rows.push(StepRow {
            n: 5,
            name: "soak list",
            status: Status::Would,
            detail: format!("append {role} to {}", p.display()),
        });
    } else {
        let text = soak_text.unwrap_or_default();
        let back = write_text_atomic(&p, &soak_with(&text, role)).and_then(|()| {
            let t = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
            let now_roles = soak_roles(&t)?;
            let count = now_roles.iter().filter(|r| r.as_str() == role).count();
            if count == 1 && now_roles.len() == have.len() + 1 {
                Ok(())
            } else {
                Err(format!(
                    "read back: {role} listed {count} times, {} roles (was {})",
                    now_roles.len(),
                    have.len()
                ))
            }
        });
        match back {
            Ok(()) => rows.push(StepRow {
                n: 5,
                name: "soak list",
                status: Status::Ok,
                detail: format!("{role} listed once; {} roles", have.len() + 1),
            }),
            Err(e) => {
                fail(&mut rows, 5, "soak list", e);
                return rep(rows, paths);
            }
        }
    }

    // --- the later slices, printed so the whole plan is visible --------------------------------
    let later = |n, name, detail: String| StepRow {
        n,
        name,
        status: Status::Later,
        detail,
    };
    if vcs.is_none() {
        rows.push(later(
            1,
            "create repo",
            format!("gh repo create {ORG}/{repo} --public (bot token)"),
        ));
        rows.push(later(
            2,
            "clone + seed + CI",
            format!(
                "clone to {}, seed, push main",
                roots.repo_dir(repo).display()
            ),
        ));
    }
    if let Some(h) = host {
        // One at a time: step 7 never runs after a failed step 6.
        match step_identity(h, roots, role, apply) {
            Ok((status, detail)) => rows.push(StepRow {
                n: 6,
                name: "synapse identity",
                status,
                detail,
            }),
            Err(e) => {
                fail(&mut rows, 6, "synapse identity", e);
                return rep(rows, paths);
            }
        }
        match step_start(h, roots, role, model, apply) {
            Ok((status, detail)) => rows.push(StepRow {
                n: 7,
                name: "start",
                status,
                detail,
            }),
            Err(e) => {
                fail(&mut rows, 7, "start", e);
                return rep(rows, paths);
            }
        }
    } else {
        rows.push(later(
            6,
            "synapse identity",
            format!(
                "synapse-provision-weekly.ps1 for {role}; home {}",
                roots.lane_home(role).display()
            ),
        ));
        rows.push(later(
            7,
            "start",
            format!("lane-restart start --role {role} --model {model} --reason lane-new"),
        ));
    }
    rows.push(later(
        8,
        "protect main (PM)",
        format!(
            "after the first CI run on main: tools/protect-main.sh {nwo} {}",
            remote::REQUIRED_CONTEXTS
                .iter()
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    ));
    rows.sort_by_key(|r| r.n);
    let mut report = rep(rows, paths);
    if !apply && !report.failed() {
        report.plan = plan_hash(roots, role, repo, model).ok();
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{tempdir, TempDir};

    fn roots(d: &TempDir) -> Roots {
        let b = d.path();
        std::fs::create_dir_all(b.join("state")).unwrap();
        std::fs::write(b.join("state").join(SOAK_FILE), "# soak\npm-assist\nfuel\n").unwrap();
        Roots::new(
            b.join("state"),
            b.join("proj"),
            b.join("homes"),
            b.join("pilot"),
            b.join("bin").join("chan"),
        )
        .unwrap()
    }

    fn go(r: &Roots, role: &str, apply: bool) -> Report {
        run_local(r, role, "zetafish", "sonnet", apply, Utc::now())
    }

    fn statuses(rep: &Report) -> Vec<(u8, Status)> {
        rep.rows
            .iter()
            .filter(|r| r.n != 0 && r.status != Status::Later)
            .map(|r| (r.n, r.status))
            .collect()
    }

    fn listing(r: &Roots) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(&r.state_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn a_relative_root_is_refused_and_every_reported_path_is_absolute() {
        let d = tempdir().unwrap();
        let ok = roots(&d);
        let bad = Roots::new(
            "rel/state".into(),
            ok.projects.clone(),
            ok.lane_homes.clone(),
            ok.pilot_home.clone(),
            ok.channel_exe.clone(),
        );
        assert!(bad
            .unwrap_err()
            .contains("state dir is not an absolute path"));
        let rep = go(&ok, "zetafish", false);
        assert!(!rep.paths.is_empty());
        assert!(rep.paths.iter().all(|p| p.is_absolute()), "{:?}", rep.paths);
    }

    #[test]
    fn a_dry_run_writes_nothing_and_apply_writes_three_files() {
        let d = tempdir().unwrap();
        let r = roots(&d);
        let before = listing(&r);
        let dry = go(&r, "zetafish", false);
        assert!(!dry.failed(), "{}", dry.render(false));
        assert_eq!(
            statuses(&dry),
            vec![(3, Status::Would), (4, Status::Would), (5, Status::Would)]
        );
        assert_eq!(listing(&r), before, "a dry run changed the state dir");
        // control: the same run with --apply does change it
        let run = go(&r, "zetafish", true);
        assert_eq!(
            statuses(&run),
            vec![(3, Status::Ok), (4, Status::Ok), (5, Status::Ok)],
            "{}",
            run.render(true)
        );
        let after = listing(&r);
        assert!(
            after.contains(&"zetafish.mcp.json".to_string())
                && after.contains(&"zetafish.launch.json".to_string())
        );
    }

    #[test]
    fn rerunning_converges_and_a_missing_artifact_is_the_only_thing_redone() {
        let d = tempdir().unwrap();
        let r = roots(&d);
        go(&r, "zetafish", true);
        let spec_bytes = std::fs::read(r.spec_path("zetafish")).unwrap();
        let again = go(&r, "zetafish", true);
        assert_eq!(
            statuses(&again),
            vec![
                (3, Status::Already),
                (4, Status::Already),
                (5, Status::Already)
            ]
        );
        assert_eq!(
            std::fs::read(r.spec_path("zetafish")).unwrap(),
            spec_bytes,
            "an already-step rewrote the spec"
        );
        std::fs::remove_file(r.spec_path("zetafish")).unwrap();
        let third = go(&r, "zetafish", true);
        assert_eq!(
            statuses(&third),
            vec![(3, Status::Already), (4, Status::Ok), (5, Status::Already)]
        );
    }

    #[test]
    fn the_spec_is_suspended_with_no_kind_reads_back_whole_and_the_launcher_refuses_it() {
        let d = tempdir().unwrap();
        let r = roots(&d);
        go(&r, "zetafish", true);
        let s = spec::read(&r.state_dir, "zetafish").unwrap();
        assert_eq!(s.state, SpecState::Suspended);
        assert!(s.stop_kind.is_none());
        assert_eq!(s.cwd, r.projects.join("zetafish").to_string_lossy());
        assert_eq!(s.launch_args[1], r.mcp_path("zetafish").to_string_lossy());
        assert!(matches!(
            start::launcher_may_start(&s),
            Err(start::StartRefusal::StopNotSoft("stop-unclassified"))
        ));
        // an unknown variant is a parse failure, not a spec (the PM's `stopped`)
        let p = r.spec_path("zetafish");
        let t = std::fs::read_to_string(&p)
            .unwrap()
            .replace("\"suspended\"", "\"stopped\"");
        std::fs::write(&p, t).unwrap();
        assert!(spec::read(&r.state_dir, "zetafish").is_err());
    }

    #[test]
    fn a_backslash_in_a_path_survives_the_json_round_trip() {
        let d = tempdir().unwrap();
        let b = d.path();
        std::fs::create_dir_all(b.join("state")).unwrap();
        std::fs::write(b.join("state").join(SOAK_FILE), "fuel\n").unwrap();
        // a literal backslash and a quote in a directory name (legal on Linux; on Windows `\` is a separator)
        let proj = b.join(if cfg!(windows) { "p r" } else { "p\\r\"q" });
        let r = Roots::new(
            b.join("state"),
            proj.clone(),
            b.join("homes"),
            b.join("pilot"),
            b.join("bin").join("chan"),
        )
        .unwrap();
        let rep = go(&r, "zetafish", true);
        assert!(!rep.failed(), "{}", rep.render(true));
        let s = spec::read(&r.state_dir, "zetafish").unwrap();
        assert_eq!(s.cwd, proj.join("zetafish").to_string_lossy());
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(r.mcp_path("zetafish")).unwrap())
                .unwrap();
        let home = v["mcpServers"]["synapse"]["env"]["SYNAPSE_HOME"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(home, fwd(&r.lane_home("zetafish")));
    }

    #[test]
    fn the_mcp_config_has_the_shape_of_a_lane_that_works() {
        let d = tempdir().unwrap();
        let r = roots(&d);
        let v = mcp_config(&r, "zetafish");
        let s = &v["mcpServers"]["synapse"];
        let keys: Vec<&str> = s.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["args", "command", "env"]);
        let args: Vec<&str> = s["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();
        assert_eq!(&args[..2], &["--role", "zetafish"]);
        assert_eq!(args[2], "--home");
        let env: Vec<&str> = s["env"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            env,
            vec!["SYNAPSE_ADDR", "SYNAPSE_DAEMON_HOME", "SYNAPSE_HOME"]
        );
        assert_eq!(s["env"]["SYNAPSE_ADDR"], SYNAPSE_ADDR);
    }

    #[test]
    fn step_0_refuses_before_anything_is_written() {
        let d = tempdir().unwrap();
        let r = roots(&d);
        let before = listing(&r);
        for (role, repo, model, why) in [
            ("pm", "x", "sonnet", "pm"),
            ("a/b", "x", "sonnet", "plain identifier"),
            ("zetafish", "a/b", "sonnet", "repository name"),
            ("zetafish", "..", "sonnet", "repository name"),
            ("zetafish", "x", "opus", "never Opus"),
            ("zetafish", "x", "claude-opus-5-5", "never Opus"),
        ] {
            let rep = run_local(&r, role, repo, model, true, Utc::now());
            assert!(
                rep.failed() && rep.rows[0].detail.contains(why),
                "{role} {repo} {model}: {}",
                rep.render(true)
            );
        }
        // a lane that has run
        std::fs::write(r.state_dir.join("zetafish.json"), "{}").unwrap();
        let rep = go(&r, "zetafish", true);
        assert!(rep.failed() && rep.rows[0].detail.contains("has run"));
        std::fs::remove_file(r.state_dir.join("zetafish.json")).unwrap();
        // an existing spec for another cwd differs: refused, and nothing else was written either
        let mut other = build_spec(&r, "zetafish", "elsewhere", "sonnet", Utc::now()).unwrap();
        other.state = SpecState::Suspended;
        spec::write_atomic(&r.state_dir, &other).unwrap();
        let before2 = listing(&r);
        let rep = go(&r, "zetafish", true);
        assert!(
            rep.failed() && rep.rows[0].detail.contains("differs"),
            "{}",
            rep.render(true)
        );
        assert_eq!(
            listing(&r),
            before2,
            "step 0 refused but something was written"
        );
        assert!(
            before.len() < before2.len(),
            "control: the earlier listing was shorter"
        );
        // a missing soak file
        std::fs::remove_file(r.soak_path()).unwrap();
        std::fs::remove_file(r.spec_path("zetafish")).unwrap();
        assert!(go(&r, "zetafish", true).rows[0]
            .detail
            .contains("not found"));
    }

    #[test]
    fn the_soak_list_keeps_its_line_ending_and_counts_only_real_entries() {
        let d = tempdir().unwrap();
        let r = roots(&d);
        // CRLF file with no final newline, and the role only in a comment
        std::fs::write(r.soak_path(), "# zetafish soon\r\nfuel").unwrap();
        let rep = go(&r, "zetafish", true);
        assert!(!rep.failed(), "{}", rep.render(true));
        assert_eq!(
            std::fs::read_to_string(r.soak_path()).unwrap(),
            "# zetafish soon\r\nfuel\r\nzetafish\r\n"
        );
        // an unsafe line anywhere fails the run, as the weekly script would
        std::fs::write(r.soak_path(), "fuel\n..\\x\n").unwrap();
        std::fs::remove_file(r.spec_path("zetafish")).unwrap();
        std::fs::remove_file(r.mcp_path("zetafish")).unwrap();
        let rep = go(&r, "zetafish", true);
        assert!(
            rep.failed() && rep.rows[0].detail.contains("unsafe"),
            "{}",
            rep.render(true)
        );
        // listed twice: step 5 refuses to guess
        std::fs::write(r.soak_path(), "zetafish\nzetafish\n").unwrap();
        let rep = go(&r, "zetafish", true);
        assert!(
            rep.failed()
                && rep
                    .rows
                    .iter()
                    .any(|x| x.n == 5 && x.status == Status::Fail),
            "{}",
            rep.render(true)
        );
    }
}
