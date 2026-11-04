// SPDX-License-Identifier: MIT OR Apache-2.0

//! The `specs` commands through the `Ctx` seam: lane-state and launch-spec files in a tempdir state
//! directory, fake process facts, a fake verified caller, and the `restart.log` rows read back.
//! The machine's state directory and ledger are never touched.

use super::*;
use crate::app::run_with;
use crate::app::test_fixtures::{fixture, live_facts};
use crate::spec;
use crate::start::CallerError;
use crate::testing::{log, state_at, RecordingFacts};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

fn ok(code: ExitCode) -> bool {
    format!("{code:?}") == format!("{:?}", ExitCode::SUCCESS)
}

fn failed(code: ExitCode) -> bool {
    format!("{code:?}") == format!("{:?}", ExitCode::FAILURE)
}

fn args(rest: &[&str]) -> Vec<OsString> {
    std::iter::once("lane-restart")
        .chain(rest.iter().copied())
        .map(OsString::from)
        .collect()
}

/// A context over `state` whose verified caller is `caller` (`None`: it cannot be verified).
fn ctx(state: &Path, caller: Option<&'static str>, facts: RecordingFacts) -> Ctx {
    let make: Box<dyn Fn() -> Result<String, CallerError>> = match caller {
        Some(role) => Box::new(move || Ok(role.to_string())),
        None => Box::new(|| Err(CallerError::NoClaudeAncestor)),
    };
    Ctx::capture(state.to_path_buf(), Ok(state.to_path_buf()), None)
        .with_facts(facts)
        .with_caller(make)
}

fn dead() -> RecordingFacts {
    RecordingFacts::new(&log())
}

fn rows(state: &Path) -> Vec<crate::ledger::Row> {
    crate::ledger::read(&state.join("restart.log"))
        .unwrap()
        .rows
}

const LAUNCH_ARGS: [&str; 8] = [
    "claude.exe",
    "prompt",
    "--name",
    "alpha",
    "--model",
    "sonnet",
    "--dangerously-load-development-channels",
    "server:claude-peers",
];

/// A lane-state file for `role` (not live), with launch arguments and a directory that exists.
fn importable(state: &Path, role: &str, cwd: &Path) {
    std::fs::create_dir_all(state).unwrap();
    let mut st = state_at(role, "s", "Stop");
    st.cwd = cwd.to_string_lossy().into_owned();
    st.launch_args = Some(LAUNCH_ARGS.iter().map(|s| s.to_string()).collect());
    std::fs::write(
        state.join(format!("{role}.json")),
        serde_json::to_string(&st).unwrap(),
    )
    .unwrap();
}

/// A lane-state file for `role` whose directory is gone (retirable), not live.
fn gone(state: &Path, role: &str) {
    std::fs::create_dir_all(state).unwrap();
    let mut st = state_at(role, "s", "Stop");
    st.cwd = state.join("no-such-dir").to_string_lossy().into_owned();
    std::fs::write(
        state.join(format!("{role}.json")),
        serde_json::to_string(&st).unwrap(),
    )
    .unwrap();
}

/// `alpha` (a live lane with a complete spec, from the shared fixture) with its spec rewritten to
/// carry `--mcp-config` pointing at a config that names the synapse server.
fn with_synapse_config(root: &Path) -> (PathBuf, PathBuf) {
    let (state, lane) = fixture(root, "alpha", "# h\n");
    let cfg = root.join("alpha.mcp.json");
    std::fs::write(&cfg, r#"{"mcpServers": {"synapse": {}}}"#).unwrap();
    let mut s = spec::read(&state, "alpha").unwrap();
    s.launch_args = [
        "claude.exe",
        "prompt",
        "--name",
        "alpha",
        "--model",
        "sonnet",
        "--mcp-config",
        &cfg.to_string_lossy(),
        "--dangerously-load-development-channels",
        "server:claude-peers",
    ]
    .map(String::from)
    .to_vec();
    spec::write_atomic(&state, &s).unwrap();
    (state, lane)
}

// ---- the PM guard, common to the three PM-only commands -------------------------------------

#[test]
fn a_non_pm_caller_is_refused_by_name_and_recorded_by_import_retire_and_synapse_only() {
    let cases: [(&str, &str, &str); 3] = [
        ("import", "import", "import specs"),
        ("retire", "retire", "retire roles"),
        ("synapse-only", "synapse-only", "rewrite specs"),
    ];
    for (sub, kind, what) in cases {
        let d = tempfile::tempdir().unwrap();
        let state = d.path().join("state");
        let mut c = ctx(&state, Some("alpha"), dead());
        let code = match sub {
            "import" => import(&mut c, None, false, false),
            "retire" => retire(&mut c, vec!["beta".into()], false),
            _ => synapse_only(
                &mut c,
                crate::cli::Roles::Named(vec!["beta".into()]),
                false,
                false,
            ),
        };
        assert!(failed(code), "{sub}");
        let (out, err) = c.captured();
        assert_eq!(out, "", "{sub}");
        assert_eq!(
            err,
            format!(
                "lane-restart specs {sub}: refused - only the PM session may {what}; this is running under \"alpha\"\n"
            ),
            "{sub}"
        );
        let rows = rows(&state);
        assert_eq!(rows.len(), 1, "{sub}");
        assert_eq!(rows[0].kind.as_deref(), Some(kind), "{sub}");
        assert_eq!(
            (rows[0].requested_by.as_str(), rows[0].acted),
            ("alpha", false),
            "{sub}"
        );
        assert!(
            rows[0]
                .outcome
                .starts_with("refused: only the PM session may"),
            "{sub}"
        );
    }
}

#[test]
fn an_unverifiable_caller_is_refused_and_recorded_as_unverified() {
    let d = tempfile::tempdir().unwrap();
    let state = d.path().join("state");
    let mut c = ctx(&state, None, dead());
    assert!(failed(import(&mut c, Some("beta".into()), false, false)));
    assert!(
        c.captured().1.contains("and the caller could not be verified: this command is not running under a claude session"),
        "{}",
        c.captured().1
    );
    let rows = rows(&state);
    assert_eq!(rows[0].requested_by, "unverified");
    assert_eq!(
        rows[0].role, "beta",
        "the target role is the ledger row's role"
    );
}

// ---- import ------------------------------------------------------------------------------

#[test]
fn import_writes_a_spec_from_the_lane_state_and_records_it() {
    let d = tempfile::tempdir().unwrap();
    let state = d.path().join("state");
    importable(&state, "alpha", d.path());
    // Dry run first: reported, nothing written, no row.
    let mut c = ctx(&state, Some("pm"), dead());
    assert!(ok(import(&mut c, None, true, false)));
    let out = c.captured().0;
    assert!(out.starts_with("DRY RUN - nothing written\n"), "{out}");
    assert!(out.contains("alpha\twould write\tstate=Crashed\n"), "{out}");
    assert!(!state.join("alpha.launch.json").exists());
    assert!(!state.join("restart.log").exists());
    // Then for real.
    let mut c = ctx(&state, Some("pm"), dead());
    assert!(ok(import(&mut c, None, false, false)));
    let out = c.captured().0;
    assert!(
        out.starts_with("IMPORT - writing launch specs (a dry run is --dry-run)\n"),
        "{out}"
    );
    assert!(out.contains("alpha\twritten\tstate=Crashed\n"), "{out}");
    assert!(state.join("alpha.launch.json").exists());
    let rows = rows(&state);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].acted);
    assert_eq!(rows[0].kind.as_deref(), Some("import"));
    assert_eq!(
        rows[0].outcome,
        "imported a launch spec from the lane state file (state=Crashed)"
    );
    // A second import leaves it alone.
    let mut c = ctx(&state, Some("pm"), dead());
    assert!(ok(import(&mut c, None, false, false)));
    assert!(c
        .captured()
        .0
        .contains("alpha\tskipped\ta launch spec already exists\n"));
}

#[test]
fn import_of_a_named_role_that_cannot_be_imported_fails_and_names_why() {
    let d = tempfile::tempdir().unwrap();
    let state = d.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    // No launch_args recorded: not guessed.
    let st = state_at("beta", "s", "Stop");
    std::fs::write(state.join("beta.json"), serde_json::to_string(&st).unwrap()).unwrap();
    let mut c = ctx(&state, Some("pm"), dead());
    assert!(failed(import(&mut c, Some("beta".into()), false, false)));
    let out = c.captured().0;
    assert!(
        out.contains("beta\trefused\tno launch_args recorded"),
        "{out}"
    );
    // The control: the same refusal for a role not NAMED is not a failure of the command.
    let mut c = ctx(&state, Some("pm"), dead());
    assert!(ok(import(&mut c, None, false, false)));
}

// ---- retire --------------------------------------------------------------------------------

#[test]
fn retire_moves_a_role_whose_directory_is_gone_and_records_it() {
    let d = tempfile::tempdir().unwrap();
    let state = d.path().join("state");
    gone(&state, "oldlane");
    let mut c = ctx(&state, Some("pm"), dead());
    assert!(ok(retire(&mut c, vec!["oldlane".into()], true)));
    let out = c.captured().0;
    assert!(out.starts_with("DRY RUN - nothing moved\n"), "{out}");
    assert!(
        out.contains("oldlane\twould retire\toldlane.json\n"),
        "{out}"
    );
    assert!(state.join("oldlane.json").exists());
    assert!(!state.join("restart.log").exists());

    let mut c = ctx(&state, Some("pm"), dead());
    assert!(ok(retire(&mut c, vec!["oldlane".into()], false)));
    let out = c.captured().0;
    assert!(out.contains("oldlane\tretired\toldlane.json\n"), "{out}");
    assert!(out.ends_with("RETIRED 1 role(s)\n"), "{out}");
    assert!(
        !state.join("oldlane.json").exists(),
        "moved out of the live state"
    );
    assert!(state.join("retired").is_dir());
    let rows = rows(&state);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].acted);
    assert_eq!(rows[0].outcome, "moved oldlane.json to retired/");
}

#[test]
fn retire_refuses_the_pm_a_missing_role_and_a_live_one_and_each_is_recorded() {
    let d = tempfile::tempdir().unwrap();
    let (state, _) = fixture(d.path(), "alpha", "# h\n"); // alpha is LIVE with these facts
    gone(&state, "pm");
    let mut c = ctx(&state, Some("pm"), live_facts());
    let roles = vec!["pm".to_string(), "ghost".to_string(), "alpha".to_string()];
    assert!(failed(retire(&mut c, roles, false)));
    let out = c.captured().0;
    assert!(
        out.contains("pm\trefused\trole pm is never retired\n"),
        "{out}"
    );
    assert!(
        out.contains("ghost\trefused\tno readable lane state"),
        "{out}"
    );
    assert!(out.contains("alpha\trefused\tcwd exists:"), "{out}");
    let rows = rows(&state);
    assert_eq!(rows.len(), 3);
    assert!(rows
        .iter()
        .all(|r| !r.acted && r.outcome.starts_with("refused: ")));
    assert!(state.join("alpha.json").exists() && state.join("pm.json").exists());
}

// ---- synapse-only --------------------------------------------------------------------------

#[test]
fn synapse_only_rewrites_the_spec_keeps_one_backup_and_records_it() {
    let d = tempfile::tempdir().unwrap();
    let (state, _) = with_synapse_config(d.path());
    let before = spec::read(&state, "alpha").unwrap().launch_args;
    let mut c = ctx(&state, Some("pm"), live_facts());
    assert!(ok(synapse_only(
        &mut c,
        crate::cli::Roles::Named(vec!["alpha".into()]),
        false,
        true
    )));
    assert!(c.captured().0.contains("DRY RUN"), "{}", c.captured().0);
    assert_eq!(
        spec::read(&state, "alpha").unwrap().launch_args,
        before,
        "a dry run writes nothing"
    );
    assert!(!state.join("restart.log").exists());

    let mut c = ctx(&state, Some("pm"), live_facts());
    assert!(ok(synapse_only(
        &mut c,
        crate::cli::Roles::Named(vec!["alpha".into()]),
        false,
        false
    )));
    let after = spec::read(&state, "alpha").unwrap().launch_args;
    assert_ne!(after, before);
    assert!(after.iter().any(|a| a == "server:synapse"), "{after:?}");
    assert!(
        !after.iter().any(|a| a == "server:claude-peers"),
        "{after:?}"
    );
    assert!(
        state.join("alpha.launch.json.pre-synapse").exists(),
        "the backup"
    );
    let rows = rows(&state);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].acted);
    assert_eq!(rows[0].kind.as_deref(), Some("synapse-only"));
    assert!(
        rows[0].outcome.starts_with("rewrote launch_args to "),
        "{}",
        rows[0].outcome
    );
}

#[test]
fn synapse_only_refuses_a_role_without_a_usable_spec_and_one_without_an_mcp_config() {
    let d = tempfile::tempdir().unwrap();
    let (state, _) = fixture(d.path(), "alpha", "# h\n"); // its spec has no --mcp-config
    let mut c = ctx(&state, Some("pm"), live_facts());
    let roles = vec!["ghost".to_string(), "alpha".to_string()];
    assert!(failed(synapse_only(
        &mut c,
        crate::cli::Roles::Named(roles),
        false,
        false
    )));
    let out = c.captured().0;
    assert!(out.contains("ghost"), "{out}");
    assert!(out.contains("no usable launch spec"), "{out}");
    assert!(out.contains("the spec has no --mcp-config"), "{out}");
    let rows = rows(&state);
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .all(|r| !r.acted && r.kind.as_deref() == Some("synapse-only")));
}

// ---- both-channels -------------------------------------------------------------------------

#[test]
fn both_channels_dry_run_is_open_to_any_caller_and_writes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let (state, _) = with_synapse_config(d.path());
    let before = spec::read(&state, "alpha").unwrap().launch_args;
    // Not the PM, and not even verifiable: a dry run is a read.
    for caller in [Some("alpha"), None] {
        let mut c = ctx(&state, caller, live_facts());
        assert!(
            ok(both_channels(
                &mut c,
                crate::cli::Roles::Named(vec!["alpha".into()]),
                true
            )),
            "{caller:?}"
        );
        let (out, err) = c.captured();
        assert_eq!(err, "", "{caller:?}");
        assert!(out.contains("DRY RUN"), "{out}");
    }
    assert_eq!(spec::read(&state, "alpha").unwrap().launch_args, before);
    assert!(
        !state.join("restart.log").exists(),
        "nothing to refuse, nothing logged"
    );
}

#[test]
fn both_channels_writing_is_pm_only_and_the_pm_write_is_recorded() {
    let d = tempfile::tempdir().unwrap();
    let (state, _) = with_synapse_config(d.path());
    let before = spec::read(&state, "alpha").unwrap().launch_args;
    // The control for the open dry run above: the same call, writing, as a non-PM.
    let mut c = ctx(&state, Some("alpha"), live_facts());
    assert!(failed(both_channels(
        &mut c,
        crate::cli::Roles::Named(vec!["alpha".into()]),
        false
    )));
    assert!(
        c.captured().1.starts_with(
            "lane-restart specs both-channels: refused - only the PM session may rewrite specs"
        ),
        "{}",
        c.captured().1
    );
    assert_eq!(
        spec::read(&state, "alpha").unwrap().launch_args,
        before,
        "the refused write changed nothing"
    );
    assert!(!state.join("alpha.launch.json.pre-both-channels").exists());
    // The PM may.
    let mut c = ctx(&state, Some("pm"), live_facts());
    assert!(ok(both_channels(
        &mut c,
        crate::cli::Roles::Named(vec!["alpha".into()]),
        false
    )));
    let after = spec::read(&state, "alpha").unwrap().launch_args;
    assert!(
        after.iter().any(|a| a == "server:claude-peers")
            && after.iter().any(|a| a == "server:synapse"),
        "{after:?}"
    );
    assert!(
        state.join("alpha.launch.json.pre-both-channels").exists(),
        "the one backup, of the spec as it was"
    );
    let rows = rows(&state);
    assert_eq!(rows.len(), 2, "the non-PM refusal, then the PM's write");
    assert!(!rows[0].acted && rows[1].acted);
    assert_eq!(rows[1].kind.as_deref(), Some("both-channels"));
}

// ---- the command line ----------------------------------------------------------------------

#[test]
fn the_command_line_reaches_each_subcommand_and_a_bad_one_does_not() {
    let d = tempfile::tempdir().unwrap();
    let state = d.path().join("state");
    gone(&state, "oldlane");
    let mut c = ctx(&state, Some("pm"), dead());
    assert!(ok(run_with(
        &mut c,
        args(&["specs", "retire", "--role", "oldlane", "--dry-run"])
    )));
    assert!(c.captured().0.contains("oldlane\twould retire"));
    let mut c = ctx(&state, Some("pm"), dead());
    assert!(ok(run_with(
        &mut c,
        args(&["specs", "import", "--dry-run"])
    )));
    assert!(c.captured().0.starts_with("DRY RUN - nothing written"));
    // The accepted calls above are the controls for these refusals (none of which touch a ledger).
    for (bad, who) in [
        (&["specs"][..], "lane-restart specs: expected `specs import"),
        (
            &["specs", "nonsense"][..],
            "lane-restart specs: expected `specs import",
        ),
        (
            &["specs", "import", "--bogus"][..],
            "lane-restart specs import: ",
        ),
        (&["specs", "retire"][..], "lane-restart specs retire: "),
        (
            &["specs", "synapse-only", "--bogus"][..],
            "lane-restart specs synapse-only: ",
        ),
        (
            &["specs", "both-channels", "--bogus"][..],
            "lane-restart specs both-channels: ",
        ),
        (
            &["specs", "audit", "--bogus"][..],
            "lane-restart specs audit: ",
        ),
    ] {
        let mut c = ctx(&state, Some("pm"), dead());
        assert!(failed(run_with(&mut c, args(bad))), "{bad:?}");
        let (out, err) = c.captured();
        assert_eq!(out, "", "{bad:?}");
        assert!(err.starts_with(who), "{bad:?}: {err}");
    }
    assert!(!state.join("restart.log").exists());
}

// ---- audit ----------------------------------------------------------------------------------

#[test]
fn audit_is_open_to_any_caller_reads_the_daemon_through_the_seam_and_writes_nothing() {
    use crate::spec_audit::Presence;
    let d = tempfile::tempdir().unwrap();
    let (state, _) = with_synapse_config(d.path());
    let ask = || {
        Ok(std::collections::BTreeMap::from([(
            "alpha".to_string(),
            Presence {
                online: false,
                queued: 2,
                oldest_enqueued_at: None,
            },
        )]))
    };
    for caller in [Some("alpha"), None] {
        let mut c = ctx(&state, caller, dead()).with_presence(ask);
        // peers-only and no identity home: failing, and the table says why.
        assert!(
            failed(run_with(&mut c, args(&["specs", "audit"]))),
            "{caller:?}"
        );
        let (out, err) = c.captured();
        assert_eq!(err, "", "{caller:?}");
        assert!(out.starts_with("role	channels"), "{out}");
        assert!(out.contains("alpha	mcp+peers+-"), "{out}");
        assert!(out.contains("peers-only"), "{out}");
        assert!(out.contains("AUDIT 1 role(s): 1 failing"), "{out}");
    }
    let mut c = ctx(&state, Some("alpha"), dead()).with_presence(ask);
    assert!(failed(run_with(
        &mut c,
        args(&["specs", "audit", "--role", "alpha", "--json"])
    )));
    let (out, _) = c.captured();
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v[0]["row"]["role"], "alpha");
    assert_eq!(v[0]["row"]["presence"]["queued"], 2);
    assert_eq!(v[0]["findings"][0]["severity"], "fail");
    assert!(!state.join("restart.log").exists(), "an audit logs nothing");
}

#[test]
fn all_covers_every_recorded_role_skips_and_names_the_refused_and_exits_nonzero_only_then() {
    let d = tempfile::tempdir().unwrap();
    let (state, _) = with_synapse_config(d.path());
    // A second role that cannot be rewritten: no mcp config file of its own.
    let (_, _) = fixture(
        d.path(),
        "beta",
        "# h
",
    );
    let mut c = ctx(&state, Some("pm"), dead());
    assert!(failed(run_with(
        &mut c,
        args(&["specs", "both-channels", "--all", "--dry-run"])
    )));
    let (out, _) = c.captured();
    assert!(out.contains("alpha	would rewrite"), "{out}");
    assert!(out.contains("beta	refused"), "{out}");
    assert!(
        out.contains("SKIPPED 1 of 2 role(s) as refused, exit 1: beta ("),
        "{out}"
    );
    // With only the rewritable role, --all exits 0 and says so.
    let d2 = tempfile::tempdir().unwrap();
    let (state2, _) = with_synapse_config(d2.path());
    let mut c = ctx(&state2, Some("pm"), dead());
    assert!(ok(run_with(
        &mut c,
        args(&["specs", "synapse-only", "--all", "--dry-run"])
    )));
    assert!(c
        .captured()
        .0
        .contains("ALL 1 role(s) handled, none refused"));
}

#[test]
fn all_and_role_are_exclusive_and_one_of_them_is_required() {
    for (cmd, bad) in [
        ("both-channels", &["--all", "--role", "a"][..]),
        ("both-channels", &[][..]),
        ("synapse-only", &["--role", "a", "--all"][..]),
        ("synapse-only", &["--strict"][..]),
    ] {
        let d = tempfile::tempdir().unwrap();
        let (state, _) = with_synapse_config(d.path());
        let mut c = ctx(&state, Some("pm"), dead());
        let mut a = vec!["specs", cmd];
        a.extend(bad);
        assert!(failed(run_with(&mut c, args(&a))), "{cmd} {bad:?}");
        let (out, err) = c.captured();
        assert_eq!(out, "", "{cmd} {bad:?}");
        assert!(err.contains("specs "), "{err}");
    }
}

// ---- set ------------------------------------------------------------------------------------

#[test]
fn set_dry_run_is_open_to_any_caller_and_writing_is_pm_only_and_recorded() {
    let d = tempfile::tempdir().unwrap();
    let (state, _) = with_synapse_config(d.path());
    let before = spec::read(&state, "alpha").unwrap();
    // a dry run is a read: any caller, even an unverifiable one
    for caller in [Some("alpha"), None] {
        let mut c = ctx(&state, caller, live_facts());
        assert!(
            ok(run_with(
                &mut c,
                args(&[
                    "specs",
                    "set",
                    "--role",
                    "alpha",
                    "--model",
                    "haiku",
                    "--dry-run"
                ])
            )),
            "{caller:?}"
        );
        let (out, err) = c.captured();
        assert_eq!(err, "", "{caller:?}");
        assert!(
            out.contains("alpha\twould set\tmodel sonnet -> haiku"),
            "{out}"
        );
    }
    assert_eq!(spec::read(&state, "alpha").unwrap(), before);
    assert!(
        !state.join("restart.log").exists(),
        "nothing refused, nothing logged"
    );
    // writing: a non-PM caller is refused by name and recorded; nothing changes
    let mut c = ctx(&state, Some("alpha"), live_facts());
    assert!(failed(run_with(
        &mut c,
        args(&["specs", "set", "--role", "alpha", "--model", "haiku"])
    )));
    assert!(c
        .captured()
        .1
        .starts_with("lane-restart specs set: refused - only the PM session may rewrite specs"));
    assert_eq!(spec::read(&state, "alpha").unwrap(), before);
    // the PM write lands and is recorded as kind "set"
    let mut c = ctx(&state, Some("pm"), live_facts());
    assert!(ok(run_with(
        &mut c,
        args(&["specs", "set", "--role", "alpha", "--model", "haiku"])
    )));
    assert_eq!(spec::read(&state, "alpha").unwrap().model, "haiku");
    assert!(state.join("alpha.launch.json.pre-set").exists());
    let l = rows(&state);
    assert_eq!(l.len(), 2);
    assert!(!l[0].acted && l[0].kind.as_deref() == Some("set"));
    assert!(l[1].acted && l[1].kind.as_deref() == Some("set"));
}

#[test]
fn set_refuses_opus_and_a_missing_selector_at_the_parser_and_all_names_the_skipped() {
    let d = tempfile::tempdir().unwrap();
    let (state, _) = with_synapse_config(d.path());
    for bad in [
        &["specs", "set", "--role", "alpha", "--model", "opus"][..],
        &[
            "specs",
            "set",
            "--role",
            "alpha",
            "--permission-mode",
            "bypassPermissions",
        ][..],
        &["specs", "set", "--model", "haiku"][..],
        &[
            "specs", "set", "--all", "--role", "alpha", "--model", "haiku",
        ][..],
        &["specs", "set", "--role", "alpha"][..],
        &[
            "specs",
            "set",
            "--role",
            "alpha",
            "--remote-control",
            "maybe",
        ][..],
    ] {
        let mut c = ctx(&state, Some("pm"), live_facts());
        assert!(failed(run_with(&mut c, args(bad))), "{bad:?}");
        let (out, err) = c.captured();
        assert_eq!(out, "", "{bad:?}");
        assert!(err.starts_with("lane-restart specs set: "), "{err}");
    }
    assert_eq!(spec::read(&state, "alpha").unwrap().model, "sonnet");
    // --all: one role, removing an argument it does not carry is refused and named, exit 1
    let mut c = ctx(&state, Some("pm"), live_facts());
    assert!(failed(run_with(
        &mut c,
        args(&["specs", "set", "--all", "--remove-arg", "--nope"])
    )));
    let (out, _) = c.captured();
    assert!(
        out.contains("SKIPPED 1 of 1 role(s) as refused, exit 1: alpha ("),
        "{out}"
    );
}
