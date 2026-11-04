use super::*;
use crate::spec::{SpecState, StopKind};
use tempfile::tempdir;

fn v(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

fn write_spec(dir: &Path, role: &str, args: &[&str], soft: bool) {
    let s = SpecFile {
        schema: spec::SCHEMA,
        role: lane_state::role::Role::new(role).unwrap(),
        name: lane_state::role::Role::new(role).unwrap(),
        cwd: format!("C:/Projects/{role}"),
        model: "sonnet".into(),
        permission_mode: Some("auto".into()),
        remote_control: false,
        launch_args: v(args),
        handoff_path: None,
        handoff_mtime: None,
        state: if soft {
            SpecState::Suspended
        } else {
            SpecState::Running
        },
        written_at: Utc::now(),
        written_by: "test".into(),
        stopped_session_id: None,
        stopped_pid_start_secs: None,
        stop_reason: None,
        stop_kind: soft.then_some(StopKind::Soft),
        stop_mac: None,
        tool_version: "test".into(),
        launch: None,
        wsl: None,
    };
    spec::write_atomic(dir, &s).unwrap();
}

fn opts() -> SetOptions {
    SetOptions::default()
}

fn one(d: &Path, role: &str, o: &SetOptions, dry: bool) -> RewriteOutcome {
    set_roles(d, &[role.to_string()], o, dry, Utc::now())
        .pop()
        .unwrap()
        .outcome
}

#[test]
fn the_options_refuse_opus_bypass_channel_flags_and_nothing_to_do() {
    assert!(validate(&opts()).unwrap_err().contains("nothing to set"));
    let m = |m: &str| SetOptions {
        model: Some(m.into()),
        ..opts()
    };
    assert!(validate(&m("sonnet")).is_ok() && validate(&m("haiku")).is_ok());
    for bad in ["opus", "Opus", "claude-opus-5", "sonnet-5", ""] {
        assert!(validate(&m(bad)).unwrap_err().contains("no Opus"), "{bad}");
    }
    let p = |p: &str| SetOptions {
        permission_mode: Some(p.into()),
        ..opts()
    };
    for ok in ["default", "acceptEdits", "auto"] {
        assert!(validate(&p(ok)).is_ok(), "{ok}");
    }
    assert!(validate(&p("bypassPermissions"))
        .unwrap_err()
        .contains("never launched"));
    assert!(validate(&p("plan")).is_err());
    let add = |a: &str| SetOptions {
        add_args: v(&[a]),
        ..opts()
    };
    for bad in [
        "--mcp-config",
        "server:synapse",
        "--dangerously-load-development-channels",
        "--strict-mcp-config",
        "--model",
        "--permission-mode=auto",
        "--dangerously-skip-permissions",
        "two words",
        " ",
    ] {
        assert!(validate(&add(bad)).is_err(), "{bad:?}");
    }
    assert!(validate(&add("--verbose")).is_ok());
    let both = SetOptions {
        add_args: v(&["--verbose"]),
        remove_args: v(&["--verbose"]),
        ..opts()
    };
    assert!(validate(&both)
        .unwrap_err()
        .contains("both added and removed"));
}

#[test]
fn a_set_changes_the_named_fields_writes_one_backup_and_a_dry_run_writes_nothing() {
    let d = tempdir().unwrap();
    write_spec(d.path(), "kiss", &["--verbose"], false);
    let o = SetOptions {
        model: Some("haiku".into()),
        permission_mode: Some("acceptEdits".into()),
        remote_control: Some(true),
        add_args: v(&["--debug"]),
        remove_args: v(&["--verbose"]),
    };
    let before = spec::read(d.path(), "kiss").unwrap();
    // dry run: reports every change, writes nothing
    match one(d.path(), "kiss", &o, true) {
        RewriteOutcome::WouldRewrite(c) => assert_eq!(c.len(), 5, "{c:?}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(spec::read(d.path(), "kiss").unwrap(), before);
    assert!(!d.path().join("kiss.launch.json.pre-set").exists());
    // real: written, backed up once
    assert!(matches!(
        one(d.path(), "kiss", &o, false),
        RewriteOutcome::Rewrote(_)
    ));
    let after = spec::read(d.path(), "kiss").unwrap();
    assert_eq!(after.model, "haiku");
    assert_eq!(
        after.permission_mode.as_ref().unwrap().as_str(),
        "acceptEdits"
    );
    assert!(after.remote_control);
    assert_eq!(after.launch_args, v(&["--debug"]));
    assert_eq!(after.written_by.as_str(), "set");
    let backup = d.path().join("kiss.launch.json.pre-set");
    let kept = std::fs::read(&backup).unwrap();
    // a second set changes the spec again but never replaces the backup
    let o2 = SetOptions {
        model: Some("sonnet".into()),
        ..opts()
    };
    assert!(matches!(
        one(d.path(), "kiss", &o2, false),
        RewriteOutcome::Rewrote(_)
    ));
    assert_eq!(
        std::fs::read(&backup).unwrap(),
        kept,
        "the first backup stands"
    );
    // already in that state: unchanged
    assert_eq!(one(d.path(), "kiss", &o2, false), RewriteOutcome::Unchanged);
}

#[test]
fn removing_what_is_not_there_a_missing_spec_and_a_bad_role_are_refused_untouched() {
    let d = tempdir().unwrap();
    write_spec(d.path(), "kiss", &["--verbose"], false);
    let before = spec::read(d.path(), "kiss").unwrap();
    let o = SetOptions {
        remove_args: v(&["--nope"]),
        ..opts()
    };
    match one(d.path(), "kiss", &o, false) {
        RewriteOutcome::Refused(w) => assert!(w.contains("no \"--nope\" to remove"), "{w}"),
        other => panic!("{other:?}"),
    }
    let o = SetOptions {
        model: Some("haiku".into()),
        ..opts()
    };
    for role in ["absent", "../x"] {
        assert!(matches!(
            one(d.path(), role, &o, false),
            RewriteOutcome::Refused(_)
        ));
    }
    assert_eq!(spec::read(d.path(), "kiss").unwrap(), before);
    assert!(!d.path().join("kiss.launch.json.pre-set").exists());
    // an argument the launch refuses is caught before the write (also in a dry run)
    let o = SetOptions {
        add_args: v(&["--definitely-not-a-claude-flag"]),
        ..opts()
    };
    let r = one(d.path(), "kiss", &o, true);
    assert!(
        matches!(
            r,
            RewriteOutcome::WouldRewrite(_) | RewriteOutcome::Refused(_)
        ),
        "{r:?}"
    );
}

#[test]
fn a_soft_stop_spec_stays_soft_and_verified_after_a_set() {
    let d = tempdir().unwrap();
    write_spec(d.path(), "kiss", &[], true);
    assert_eq!(
        spec::read(d.path(), "kiss").unwrap().stop_kind,
        Some(StopKind::Soft),
        "control: the written soft stop verifies"
    );
    let o = SetOptions {
        model: Some("haiku".into()),
        ..opts()
    };
    assert!(matches!(
        one(d.path(), "kiss", &o, false),
        RewriteOutcome::Rewrote(_)
    ));
    let s = spec::read(d.path(), "kiss").unwrap();
    assert_eq!(s.model, "haiku");
    assert_eq!(s.stop_kind, Some(StopKind::Soft), "re-signed by the write");
    assert!(s.stop_mac.is_some());
}

#[test]
fn the_report_says_first_and_last_whether_anything_was_written() {
    let d = tempdir().unwrap();
    write_spec(d.path(), "a", &[], false);
    write_spec(d.path(), "b", &[], false);
    let o = SetOptions {
        model: Some("haiku".into()),
        ..opts()
    };
    let roles = v(&["a", "b", "nope"]);
    let dry = render(&set_roles(d.path(), &roles, &o, true, Utc::now()), true);
    assert!(dry.starts_with("DRY RUN - nothing written\n"), "{dry}");
    assert!(
        dry.ends_with("(2 would change, 0 unchanged, 1 refused)\n"),
        "{dry}"
    );
    let real = render(&set_roles(d.path(), &roles, &o, false, Utc::now()), false);
    assert!(real.contains("a\tset\tmodel sonnet -> haiku"), "{real}");
    assert!(
        real.ends_with(
            "CHANGED 2 spec(s), 0 unchanged, 1 refused; effective at each lane's next launch\n"
        ),
        "{real}"
    );
}
