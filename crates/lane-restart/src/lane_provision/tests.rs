use super::*;
use crate::lane_new_host::ProvisionCounts;
use crate::spec_synapse::{DEV_CHANNELS, PEERS};
use std::cell::RefCell;
use std::path::Path;
use tempfile::tempdir;

fn roots(base: &Path) -> Roots {
    std::fs::create_dir_all(base.join("state")).unwrap();
    Roots::new(
        base.join("state"),
        base.join("proj"),
        base.join("homes"),
        base.join("pilot"),
        base.join("bin").join("chan"),
    )
    .unwrap()
}

fn write_spec(dir: &Path, role: &str, args: &[&str]) {
    let s = spec::SpecFile {
        schema: spec::SCHEMA,
        role: lane_state::role::Role::new(role).unwrap(),
        name: lane_state::role::Role::new(role).unwrap(),
        cwd: format!("C:/Projects/{role}"),
        model: "sonnet".into(),
        permission_mode: None,
        remote_control: false,
        launch_args: args.iter().map(|s| s.to_string()).collect(),
        handoff_path: None,
        handoff_mtime: None,
        state: spec::SpecState::Suspended,
        written_at: Utc::now(),
        written_by: "test".into(),
        stopped_session_id: None,
        stopped_pid_start_secs: None,
        stop_reason: None,
        stop_kind: None,
        stop_mac: None,
        tool_version: "test".into(),
        launch: None,
        wsl: None,
    };
    spec::write_atomic(dir, &s).unwrap();
}

/// Records its calls; its `provision` leaves what the real script leaves: a certificate and the
/// announce file in the role's lane home.
struct FakeHost {
    homes: PathBuf,
    calls: RefCell<Vec<String>>,
}

impl FakeHost {
    fn new(r: &Roots) -> FakeHost {
        FakeHost {
            homes: r.lane_homes.clone(),
            calls: RefCell::new(Vec::new()),
        }
    }
    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl Host for FakeHost {
    fn disk_floor(&self, _: &Path) -> Result<(), String> {
        Ok(())
    }
    fn provision(&self, roles_file: &Path) -> Result<ProvisionCounts, String> {
        let text = std::fs::read_to_string(roles_file).map_err(|e| e.to_string())?;
        let role = text.trim().to_string();
        self.calls.borrow_mut().push(format!("provision {role}"));
        let home = self.homes.join(&role);
        std::fs::create_dir_all(home.join("roles").join(&role)).unwrap();
        std::fs::write(home.join("roles").join(&role).join("cert.pem"), "cert").unwrap();
        std::fs::write(home.join("synapsed.json"), "{}").unwrap();
        Ok(ProvisionCounts {
            ok: 0,
            created: 1,
            renewed: 0,
            refused: 0,
            other: 0,
        })
    }
    fn start(&self, _: &str, _: &str) -> Result<i32, String> {
        self.calls.borrow_mut().push("START".into());
        Ok(0)
    }
    fn claude_alive(&self, _: u32) -> bool {
        false
    }
}

const PEERS_ONLY: &[&str] = &["--model", "sonnet", DEV_CHANNELS, PEERS];

fn status(r: &Report, n: u8) -> Status {
    r.rows.iter().find(|x| x.n == n).unwrap().status
}

fn listing(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn a_dry_run_plans_every_step_prints_the_hash_and_writes_and_runs_nothing() {
    let d = tempdir().unwrap();
    let r = roots(d.path());
    write_spec(&r.state_dir, "bitman", PEERS_ONLY);
    let host = FakeHost::new(&r);
    let before = listing(&r.state_dir);
    let rep = run(&r, Some(&host), "bitman", false, Utc::now());
    assert!(
        !rep.failed(),
        "{}",
        rep.render_as("lane-provision", "x", false)
    );
    assert_eq!(status(&rep, 3), Status::Would);
    assert_eq!(status(&rep, 6), Status::Would);
    assert_eq!(status(&rep, 4), Status::Would);
    assert_eq!(rep.plan.as_deref(), Some(plan_hash(&r, "bitman").as_str()));
    assert_eq!(listing(&r.state_dir), before, "nothing written");
    assert!(host.calls().is_empty(), "nothing run");
}

#[test]
fn apply_needs_the_plan_for_exactly_this_role_and_otherwise_does_nothing() {
    let d = tempdir().unwrap();
    let r = roots(d.path());
    write_spec(&r.state_dir, "bitman", PEERS_ONLY);
    let host = FakeHost::new(&r);
    let before = listing(&r.state_dir);
    let other = plan_hash(&r, "fmp-rs");
    for plan in [None, Some("0000000000000000"), Some(other.as_str())] {
        let rep = run_apply(&r, Some(&host), "bitman", plan, Utc::now());
        assert!(rep.failed(), "{plan:?}");
        assert_eq!(rep.rows[0].name, "plan");
    }
    assert_eq!(listing(&r.state_dir), before);
    assert!(host.calls().is_empty());
}

#[test]
fn apply_writes_the_config_provisions_once_brings_the_spec_to_the_full_form_and_never_starts() {
    let d = tempdir().unwrap();
    let r = roots(d.path());
    write_spec(&r.state_dir, "bitman", PEERS_ONLY);
    let host = FakeHost::new(&r);
    let plan = plan_hash(&r, "bitman");
    let rep = run_apply(&r, Some(&host), "bitman", Some(&plan), Utc::now());
    assert!(
        !rep.failed(),
        "{}",
        rep.render_as("lane-provision", "x", true)
    );
    for n in [3, 6, 4] {
        assert_eq!(status(&rep, n), Status::Ok, "step {n}");
    }
    assert_eq!(host.calls(), vec!["provision bitman"], "never START");
    // the config is the standard one, read back
    let t = std::fs::read_to_string(r.state_dir.join("bitman.mcp.json")).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&t).unwrap(),
        mcp_config(&r, "bitman")
    );
    // the spec now reaches Synapse, with one backup of the old one
    let s = spec::read(&r.state_dir, "bitman").unwrap();
    assert!(
        crate::spec_channels::check(&s.launch_args).is_ok(),
        "{:?}",
        s.launch_args
    );
    assert!(r
        .state_dir
        .join("bitman.launch.json.pre-both-channels")
        .exists());
}

#[test]
fn rerunning_converges_every_step_is_already_and_nothing_is_provisioned_again() {
    let d = tempdir().unwrap();
    let r = roots(d.path());
    write_spec(&r.state_dir, "bitman", PEERS_ONLY);
    let host = FakeHost::new(&r);
    let plan = plan_hash(&r, "bitman");
    assert!(!run_apply(&r, Some(&host), "bitman", Some(&plan), Utc::now()).failed());
    let again = run_apply(&r, Some(&host), "bitman", Some(&plan), Utc::now());
    assert!(!again.failed());
    for n in [3, 6, 4] {
        assert_eq!(status(&again, n), Status::Already, "step {n}");
    }
    assert_eq!(
        host.calls(),
        vec!["provision bitman"],
        "provisioned once in all"
    );
}

#[test]
fn a_role_without_a_spec_the_pm_and_a_bad_name_are_refused_at_step_0() {
    let d = tempdir().unwrap();
    let r = roots(d.path());
    let host = FakeHost::new(&r);
    write_spec(&r.state_dir, "pm", PEERS_ONLY);
    for role in ["nospec", "pm", "../x", ""] {
        let rep = run(&r, Some(&host), role, false, Utc::now());
        assert!(rep.failed(), "{role:?}");
        assert_eq!(rep.rows.len(), 1, "{role:?}");
        assert_eq!(rep.rows[0].n, 0);
        assert!(rep.plan.is_none(), "a failed dry run prints no plan");
    }
    assert!(host.calls().is_empty());
    assert!(!r.state_dir.join("nospec.mcp.json").exists());
}

#[test]
fn a_conflicting_mcp_json_is_not_overwritten_and_a_working_own_setup_is_kept() {
    let d = tempdir().unwrap();
    let r = roots(d.path());
    let host = FakeHost::new(&r);
    // conflict: a different file that names no synapse server
    write_spec(&r.state_dir, "bitman", PEERS_ONLY);
    let mcp = r.state_dir.join("bitman.mcp.json");
    std::fs::write(&mcp, r#"{"mcpServers":{"other":{}}}"#).unwrap();
    let rep = run(&r, Some(&host), "bitman", false, Utc::now());
    assert!(rep.failed());
    assert!(
        rep.rows[0].detail.contains("not overwritten"),
        "{:?}",
        rep.rows
    );
    assert_eq!(
        std::fs::read_to_string(&mcp).unwrap(),
        r#"{"mcpServers":{"other":{}}}"#
    );
    // own setup: names synapse and its identity directory exists (a shared-home lane)
    write_spec(&r.state_dir, "kiss", PEERS_ONLY);
    let home = d.path().join("shared");
    std::fs::create_dir_all(home.join("roles").join("kiss")).unwrap();
    std::fs::write(
        r.state_dir.join("kiss.mcp.json"),
        format!(
            r#"{{"mcpServers":{{"synapse":{{"args":["--role","kiss","--home","{}"]}}}}}}"#,
            home.to_string_lossy().replace('\\', "/")
        ),
    )
    .unwrap();
    let plan = plan_hash(&r, "kiss");
    let rep = run_apply(&r, Some(&host), "kiss", Some(&plan), Utc::now());
    assert!(
        !rep.failed(),
        "{}",
        rep.render_as("lane-provision", "x", true)
    );
    assert_eq!(status(&rep, 3), Status::Already);
    assert_eq!(status(&rep, 6), Status::Already);
    assert_eq!(status(&rep, 4), Status::Ok);
    assert!(
        host.calls().is_empty(),
        "the lane's own identity is not re-provisioned"
    );
}
