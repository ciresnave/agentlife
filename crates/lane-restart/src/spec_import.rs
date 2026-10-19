// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart specs import` - RESTART-TOOL-DESIGN.md 14.9: build `<role>.launch.json` from a
//! lane's `.lane-state/<role>.json` for each role that has no spec, so a lane that ended before
//! it ever launched under a build that writes specs (baracuda, 2026-10-11) becomes startable.
//!
//! Read-only on every state file. Never overwrites an existing spec (even an unreadable one:
//! that is somebody's evidence). A spec is written only if it is complete, which is to say only
//! if `prepare_launch` would accept it; the rest are listed with the reason, never dropped.
//!
//! An imported spec is labelled `written_by: "import"`: its `launch_args` came from the OS-read
//! argv of the lane's last launch, the best evidence there is, and the first real stop or start
//! replaces it with a verified one.

use crate::facts::SystemFacts;
use crate::launch::LaunchSpec;
use crate::relaunch::{resolve_launch_model, valid_identifier};
use crate::spec::{self, SpecInputs, SpecState, WrittenBy};
use crate::state;
use chrono::{DateTime, Utc};
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub enum ImportOutcome {
    /// Written, in this state.
    Written(SpecState),
    /// `--dry-run`: would be written, in this state.
    WouldWrite(SpecState),
    /// A spec file is already there. Left exactly as it is.
    AlreadyHasSpec,
    /// Not importable; the reason is the whole answer.
    Refused(String),
}

#[derive(Debug, PartialEq, Eq)]
pub struct ImportRow {
    pub role: String,
    pub outcome: ImportOutcome,
    /// The role whose cwd encloses this role's cwd (`spec::nesting`): a label on the row, never a
    /// reason to refuse. `None` when the role is not inside another role's directory.
    pub nested_in: Option<String>,
}

fn roles_in(dir: &Path) -> Vec<String> {
    let mut roles = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if spec::is_spec_file_name(&name) {
                continue;
            }
            let Some(stem) = name.strip_suffix(".json") else {
                continue;
            };
            // only files that really are lane states: `status` already lists the others
            if valid_identifier(stem) && state::load(dir, stem).is_ok() {
                roles.push(stem.to_string());
            }
        }
    }
    roles.sort();
    roles
}

fn import_one(
    dir: &Path,
    role: &str,
    facts: &dyn SystemFacts,
    policy_default: &str,
    dry_run: bool,
    now: DateTime<Utc>,
) -> ImportOutcome {
    if dir.join(format!("{role}{}", spec::FILE_SUFFIX)).exists() {
        return ImportOutcome::AlreadyHasSpec;
    }
    let st = match state::load(dir, role) {
        Ok(s) => s,
        Err(e) => return ImportOutcome::Refused(e.to_string()),
    };
    if st.launch_args.as_ref().is_none_or(Vec::is_empty) {
        return ImportOutcome::Refused(
            "no launch_args recorded: how this lane was launched is unknown, and is not guessed"
                .to_string(),
        );
    }
    if !Path::new(&st.cwd).is_dir() {
        return ImportOutcome::Refused(format!("cwd missing: {}", st.cwd));
    }
    // Opus is refused, not replaced: an import records what was; `lane-start` is what pins.
    let model = match resolve_launch_model(st.launch_args.as_ref(), policy_default, None) {
        Ok(m) => m,
        Err(e) => return ImportOutcome::Refused(e.to_string()),
    };
    let spec_state = if spec::state_process_is_live(&st, facts) {
        SpecState::Running
    } else {
        SpecState::Crashed
    };
    let launch = match LaunchSpec::try_from(&st) {
        Ok(l) => l,
        Err(e) => return ImportOutcome::Refused(e.to_string()),
    };
    let file = match spec::build(&SpecInputs {
        launch: &launch,
        model: &model,
        session_id: Some(&st.session_id),
        pid_start_secs: st.pid_start_secs,
        state: spec_state,
        written_by: WrittenBy::Import,
        now,
    }) {
        Ok(f) => f,
        Err(e) => return ImportOutcome::Refused(e.to_string()),
    };
    if let Err(e) = file.check() {
        return ImportOutcome::Refused(e.to_string());
    }
    if dry_run {
        return ImportOutcome::WouldWrite(spec_state);
    }
    match spec::write_atomic(dir, &file) {
        Ok(()) => ImportOutcome::Written(spec_state),
        Err(e) => ImportOutcome::Refused(e.to_string()),
    }
}

/// One row per role with a lane state file (or just `only_role`).
pub fn import_specs(
    dir: &Path,
    only_role: Option<&str>,
    facts: &dyn SystemFacts,
    policy_default: &str,
    dry_run: bool,
    now: DateTime<Utc>,
) -> Vec<ImportRow> {
    let roles = match only_role {
        Some(r) => vec![r.to_string()],
        None => roles_in(dir),
    };
    let nested = spec::nested_roles(dir, &roles_in(dir));
    roles
        .into_iter()
        .map(|role| {
            let outcome = if !valid_identifier(&role) {
                ImportOutcome::Refused(format!("not a plain role identifier: {role:?}"))
            } else {
                import_one(dir, &role, facts, policy_default, dry_run, now)
            };
            let nested_in = nested.get(&role).cloned();
            ImportRow {
                role,
                outcome,
                nested_in,
            }
        })
        .collect()
}

/// The key refused rows are grouped by: the reason without the part that differs per role (the
/// path of a `cwd missing:` row), so thirty vanished worktrees are one line, not thirty.
fn reason_key(why: &str) -> &str {
    if why.starts_with("cwd missing:") {
        "cwd missing"
    } else {
        why
    }
}

/// `\tnested in <role>` for a row whose cwd is inside another role's cwd (a label: the row is
/// still written, because a legitimate nested lane is indistinguishable by path alone).
fn nested_label(r: &ImportRow) -> String {
    r.nested_in
        .as_deref()
        .map(|p| format!("\tnested in {p} (its cwd is inside that role's; may not be a lane)"))
        .unwrap_or_default()
}

/// The report `specs import` prints. The first and last lines say whether anything was written
/// (`DRY RUN - nothing written` / `WROTE n spec(s)`), so a relayed report cannot be misread. Every
/// refused role is still listed with its reason: rows that share a reason are grouped onto one
/// line (count and role names) unless `verbose`, and a group of one keeps the per-role row.
pub fn render(rows: &[ImportRow], dry_run: bool, verbose: bool) -> String {
    use std::collections::BTreeMap;
    let mut out = String::new();
    out.push_str(if dry_run {
        "DRY RUN - nothing written\n"
    } else {
        "IMPORT - writing launch specs (a dry run is --dry-run)\n"
    });
    let mut refused: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
    let (mut wrote, mut would) = (0usize, 0usize);
    for r in rows {
        match &r.outcome {
            ImportOutcome::Written(s) => {
                wrote += 1;
                out.push_str(&format!(
                    "{}\twritten\tstate={s:?}{}\n",
                    r.role,
                    nested_label(r)
                ));
            }
            ImportOutcome::WouldWrite(s) => {
                would += 1;
                out.push_str(&format!(
                    "{}\twould write\tstate={s:?}{}\n",
                    r.role,
                    nested_label(r)
                ));
            }
            ImportOutcome::AlreadyHasSpec => {
                out.push_str(&format!(
                    "{}\tskipped\ta launch spec already exists\n",
                    r.role
                ));
            }
            ImportOutcome::Refused(why) => {
                refused
                    .entry(reason_key(why))
                    .or_default()
                    .push((&r.role, why));
            }
        }
    }
    for (key, group) in &refused {
        if verbose || group.len() == 1 {
            for (role, why) in group {
                out.push_str(&format!("{role}\trefused\t{why}\n"));
            }
        } else {
            let names: Vec<&str> = group.iter().map(|(r, _)| *r).collect();
            out.push_str(&format!(
                "refused\t{} role(s)\t{key}: {}\n",
                group.len(),
                names.join(", ")
            ));
        }
    }
    out.push_str(&format!("({} role(s) considered)\n", rows.len()));
    if dry_run {
        out.push_str(&format!(
            "DRY RUN - nothing written ({would} would be written)\n"
        ));
    } else {
        out.push_str(&format!("WROTE {wrote} spec(s)\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{role_status, Lifecycle};
    use crate::testing::{log, state_at, RecordingFacts};
    use tempfile::tempdir;

    fn args(v: &[&str]) -> Option<Vec<String>> {
        Some(v.iter().map(|s| s.to_string()).collect())
    }

    /// A state file for `role`, launched from `cwd`, in the shape the hooks write it.
    fn state_file(dir: &Path, role: &str, cwd: &Path, pid: u32, launch_args: Option<Vec<String>>) {
        let mut st = state_at(role, "dead-session", "PreToolUse");
        st.pid = pid;
        st.pid_start_secs = Some(100);
        st.cwd = cwd.to_string_lossy().into_owned();
        st.launch_args = launch_args;
        std::fs::write(
            dir.join(format!("{role}.json")),
            serde_json::to_string(&st).unwrap(),
        )
        .unwrap();
    }

    fn baracuda_args() -> Option<Vec<String>> {
        args(&[
            "claude.exe",
            "--dangerously-load-development-channels",
            "server:claude-peers",
            "--model",
            "sonnet",
        ])
    }

    fn run(d: &Path, role: Option<&str>, dry: bool) -> Vec<ImportRow> {
        let facts = RecordingFacts::new(&log());
        import_specs(d, role, &facts, "sonnet", dry, Utc::now())
    }

    #[test]
    fn a_dead_lane_with_launch_args_becomes_a_crashed_startable_spec() {
        let d = tempdir().unwrap();
        let cwd = tempdir().unwrap();
        state_file(d.path(), "baracuda", cwd.path(), 19184, baracuda_args());
        let rows = run(d.path(), None, false);
        assert_eq!(
            rows,
            vec![ImportRow {
                role: "baracuda".into(),
                outcome: ImportOutcome::Written(SpecState::Crashed),
                nested_in: None,
            }]
        );
        let s = spec::read(d.path(), "baracuda").unwrap();
        assert_eq!(s.written_by, WrittenBy::Import);
        assert_eq!(s.state, SpecState::Crashed);
        assert_eq!(
            s.launch_args,
            vec![
                "--dangerously-load-development-channels",
                "server:claude-peers"
            ]
        );
        // the point of the whole command: it is startable now, and the state file is untouched
        let st = role_status(d.path(), "baracuda", &RecordingFacts::new(&log()));
        assert_eq!((st.state, st.startable), (Lifecycle::Crashed, true));
    }

    #[test]
    fn a_live_lane_is_imported_as_running() {
        let d = tempdir().unwrap();
        let cwd = tempdir().unwrap();
        state_file(d.path(), "agentlife", cwd.path(), 55, baracuda_args());
        let mut facts = RecordingFacts::new(&log());
        facts.live_pid = Some((55, 100));
        let rows = import_specs(d.path(), None, &facts, "sonnet", false, Utc::now());
        assert_eq!(rows[0].outcome, ImportOutcome::Written(SpecState::Running));
    }

    #[test]
    fn an_existing_spec_is_never_overwritten_not_even_an_unreadable_one() {
        let d = tempdir().unwrap();
        let cwd = tempdir().unwrap();
        state_file(d.path(), "baracuda", cwd.path(), 1, baracuda_args());
        let existing = d.path().join("baracuda.launch.json");
        std::fs::write(&existing, "not json, somebody's evidence").unwrap();
        let rows = run(d.path(), None, false);
        assert_eq!(rows[0].outcome, ImportOutcome::AlreadyHasSpec);
        assert_eq!(
            std::fs::read_to_string(&existing).unwrap(),
            "not json, somebody's evidence"
        );
    }

    #[test]
    fn no_launch_args_is_listed_with_the_reason_and_nothing_is_written() {
        let d = tempdir().unwrap();
        let cwd = tempdir().unwrap();
        state_file(d.path(), "pm", cwd.path(), 1, None);
        let rows = run(d.path(), None, false);
        let ImportOutcome::Refused(why) = &rows[0].outcome else {
            panic!("{rows:?}")
        };
        assert!(why.contains("no launch_args"), "{why}");
        assert!(!d.path().join("pm.launch.json").exists());
    }

    #[test]
    fn a_missing_cwd_is_listed_not_written() {
        let d = tempdir().unwrap();
        let gone = d.path().join("no-such-dir");
        state_file(d.path(), "baracuda", &gone, 1, baracuda_args());
        let rows = run(d.path(), None, false);
        let ImportOutcome::Refused(why) = &rows[0].outcome else {
            panic!("{rows:?}")
        };
        assert!(why.contains("cwd missing"), "{why}");
    }

    /// An Opus pin is refused at import, not rewritten into a spec that says Sonnet.
    #[test]
    fn an_opus_launch_is_listed_not_laundered_into_a_sonnet_spec() {
        let d = tempdir().unwrap();
        let cwd = tempdir().unwrap();
        state_file(
            d.path(),
            "baracuda",
            cwd.path(),
            1,
            args(&["claude.exe", "--model", "claude-opus-5-5"]),
        );
        let rows = run(d.path(), None, false);
        assert!(
            matches!(rows[0].outcome, ImportOutcome::Refused(_)),
            "{rows:?}"
        );
        assert!(!d.path().join("baracuda.launch.json").exists());
    }

    #[test]
    fn inline_mcp_json_is_listed_and_never_stored() {
        let d = tempdir().unwrap();
        let cwd = tempdir().unwrap();
        state_file(
            d.path(),
            "baracuda",
            cwd.path(),
            1,
            args(&[
                "claude.exe",
                "--mcp-config",
                r#"{"env":{"TOKEN":"s3cr3t"}}"#,
            ]),
        );
        let rows = run(d.path(), None, false);
        assert!(
            matches!(rows[0].outcome, ImportOutcome::Refused(_)),
            "{rows:?}"
        );
        assert!(!d.path().join("baracuda.launch.json").exists());
    }

    #[test]
    fn a_dry_run_reports_and_writes_nothing() {
        let d = tempdir().unwrap();
        let cwd = tempdir().unwrap();
        state_file(d.path(), "baracuda", cwd.path(), 1, baracuda_args());
        let rows = run(d.path(), None, true);
        assert_eq!(
            rows[0].outcome,
            ImportOutcome::WouldWrite(SpecState::Crashed)
        );
        assert!(!d.path().join("baracuda.launch.json").exists());
    }

    #[test]
    fn only_the_named_role_is_imported() {
        let d = tempdir().unwrap();
        let cwd = tempdir().unwrap();
        state_file(d.path(), "baracuda", cwd.path(), 1, baracuda_args());
        state_file(d.path(), "other", cwd.path(), 2, baracuda_args());
        let rows = run(d.path(), Some("baracuda"), false);
        assert_eq!(rows.len(), 1);
        assert!(d.path().join("baracuda.launch.json").exists());
        assert!(!d.path().join("other.launch.json").exists());
    }

    #[test]
    fn files_that_are_not_lane_states_are_not_roles() {
        let d = tempdir().unwrap();
        std::fs::write(d.path().join("hf-config.json"), "{}").unwrap();
        std::fs::write(d.path().join("model-policy.json"), r#"{"model":"haiku"}"#).unwrap();
        assert_eq!(run(d.path(), None, false), vec![]);
    }

    #[test]
    fn a_named_role_with_no_state_file_is_listed() {
        let d = tempdir().unwrap();
        let rows = run(d.path(), Some("ghost"), false);
        assert!(
            matches!(rows[0].outcome, ImportOutcome::Refused(_)),
            "{rows:?}"
        );
    }

    fn row(role: &str, outcome: ImportOutcome) -> ImportRow {
        ImportRow {
            role: role.into(),
            outcome,
            nested_in: None,
        }
    }

    #[test]
    fn a_nested_role_is_still_written_and_the_row_names_its_parent() {
        let d = tempdir().unwrap();
        let outer = tempdir().unwrap();
        let inner = outer.path().join("apps").join("api");
        std::fs::create_dir_all(&inner).unwrap();
        state_file(d.path(), "outer", outer.path(), 19184, baracuda_args());
        state_file(d.path(), "api", &inner, 19185, baracuda_args());
        let rows = run(d.path(), None, true);
        let api = rows.iter().find(|r| r.role == "api").unwrap();
        let top = rows.iter().find(|r| r.role == "outer").unwrap();
        assert_eq!(api.nested_in.as_deref(), Some("outer"), "{rows:?}");
        assert_eq!(top.nested_in, None, "{rows:?}");
        assert!(
            matches!(api.outcome, ImportOutcome::WouldWrite(_)),
            "{rows:?}"
        );
        let text = render(&rows, true, false);
        assert!(
            text.contains("api\twould write\tstate=Crashed\tnested in outer"),
            "{text}"
        );
        assert!(
            text.contains("outer\twould write\tstate=Crashed\n"),
            "{text}"
        );
    }

    fn refused(role: &str, why: &str) -> ImportRow {
        row(role, ImportOutcome::Refused(why.into()))
    }

    #[test]
    fn the_report_says_first_and_last_whether_anything_was_written() {
        let rows = vec![row("a", ImportOutcome::WouldWrite(SpecState::Running))];
        let dry = render(&rows, true, false);
        assert_eq!(dry.lines().next(), Some("DRY RUN - nothing written"));
        assert_eq!(
            dry.lines().last(),
            Some("DRY RUN - nothing written (1 would be written)")
        );
        assert!(!dry.contains("WROTE"));
        let real = render(
            &[row("a", ImportOutcome::Written(SpecState::Running))],
            false,
            false,
        );
        assert!(real.starts_with("IMPORT - writing launch specs"));
        assert_eq!(real.lines().last(), Some("WROTE 1 spec(s)"));
        assert!(!real.contains("DRY RUN"));
    }

    #[test]
    fn refused_rows_with_one_reason_are_grouped_not_dropped() {
        let rows = vec![
            refused("w1", "cwd missing: C:/gone/w1"),
            refused("w2", "cwd missing: C:/gone/w2"),
            refused("w3", "cwd missing: C:/gone/w3"),
            refused("x", "no launch_args recorded: unknown"),
        ];
        let text = render(&rows, true, false);
        assert!(
            text.contains("refused\t3 role(s)\tcwd missing: w1, w2, w3\n"),
            "{text}"
        );
        // A group of one keeps the full per-role row with the reason.
        assert!(
            text.contains("x\trefused\tno launch_args recorded: unknown\n"),
            "{text}"
        );
        assert!(text.contains("(4 role(s) considered)"));
        // Every role is named somewhere.
        for r in ["w1", "w2", "w3", "x"] {
            assert!(text.contains(r), "{r} missing from {text}");
        }
        // verbose: one row per role with its own path.
        let v = render(&rows, true, true);
        assert!(v.contains("w2\trefused\tcwd missing: C:/gone/w2\n"), "{v}");
        assert!(!v.contains("3 role(s)"));
    }
}
