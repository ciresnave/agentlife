// SPDX-License-Identifier: MIT OR Apache-2.0

//! `approvals` through the `Ctx` seam: a config file under a test home directory and a fake
//! approvals repo, so nothing touches the network or the real `~/.overmind`.

use super::*;
use crate::app::run_with;
use crate::approvals::{DirEntry, RepoReader};
use std::collections::BTreeMap;
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

const PROVENANCE: &str = r#"{
    "approved_by": "CireSnave",
    "approvals": [{"said_on": "2026-09-18", "quote": "Proceed!", "approving": "a test"}]
}"#;

fn handler_json(id: &str, roles: &str, expires: &str) -> String {
    format!(
        r#"{{
            "id": "{id}",
            "match": {{"text_anchors": ["SECURITY CONFIRMATION"], "fields": {{}}}},
            "action": "1\r",
            "scope": {{ "roles": {roles} }},
            "provenance": {PROVENANCE},
            "expires_at": {expires}
        }}"#
    )
}

/// The tip commit of the fake repo: the loader refuses anything but a 40-hex SHA.
const TIP: &str = "0123456789abcdef0123456789abcdef01234567";

/// A repo with the given `(file name, contents)` pairs at the tip [`TIP`].
struct FakeRepo(BTreeMap<String, String>);

impl RepoReader for FakeRepo {
    fn default_branch_commit(&self, _repo: &str) -> Result<String, String> {
        Ok(TIP.to_string())
    }
    fn list_dir(&self, _repo: &str, _path: &str, _commit: &str) -> Result<Vec<DirEntry>, String> {
        Ok(self
            .0
            .iter()
            .map(|(name, body)| DirEntry {
                name: name.clone(),
                kind: "file".to_string(),
                size: body.len() as u64,
            })
            .collect())
    }
    fn read_file(&self, _repo: &str, path: &str, _commit: &str) -> Result<Vec<u8>, String> {
        let name = path.rsplit('/').next().unwrap();
        self.0
            .get(name)
            .map(|b| b.clone().into_bytes())
            .ok_or_else(|| format!("no such file {name}"))
    }
}

/// A repo that cannot be reached.
struct Down;
impl RepoReader for Down {
    fn default_branch_commit(&self, _repo: &str) -> Result<String, String> {
        Err("network is down".to_string())
    }
    fn list_dir(&self, _: &str, _: &str, _: &str) -> Result<Vec<DirEntry>, String> {
        unreachable!("not asked")
    }
    fn read_file(&self, _: &str, _: &str, _: &str) -> Result<Vec<u8>, String> {
        unreachable!("not asked")
    }
}

fn home_with_config(root: &Path) -> PathBuf {
    let home = root.join("home");
    std::fs::create_dir_all(home.join(".overmind")).unwrap();
    std::fs::write(
        home.join(".overmind").join("lane-restart.json"),
        r#"{"approvals": {"repo": "ciresnave/approvals", "path": ".lane/approvals"}}"#,
    )
    .unwrap();
    home
}

fn ctx(root: &Path, home: Option<PathBuf>, repo: impl RepoReader + 'static) -> Ctx {
    Ctx::capture(root.join("state"), Ok(root.to_path_buf()), None)
        .with_home(home)
        .with_repo_reader(repo)
}

fn repo_of(files: &[(&str, String)]) -> FakeRepo {
    FakeRepo(
        files
            .iter()
            .map(|(n, b)| (n.to_string(), b.clone()))
            .collect(),
    )
}

#[test]
fn an_active_approval_is_listed_with_its_hash_roles_and_expiry() {
    let d = tempfile::tempdir().unwrap();
    let home = home_with_config(d.path());
    let repo = repo_of(&[("h1.json", handler_json("h1", r#"["overmind"]"#, "null"))]);
    let mut ctx = ctx(d.path(), Some(home), repo);
    assert!(ok(run(&mut ctx, None)));
    let (out, err) = ctx.captured();
    assert_eq!(err, "");
    let lines: Vec<&str> = out.lines().collect();
    assert!(
        lines[0].starts_with(
            "approvals: 1 active, 0 refused, from ciresnave/approvals:.lane/approvals@0123456789ab ["
        ),
        "{out}"
    );
    assert!(lines[1].starts_with("  active h1 "), "{out}");
    assert!(
        lines[1].ends_with(" roles=[\"overmind\"] expires=never"),
        "{out}"
    );
}

#[test]
fn role_marks_whether_each_approval_applies_to_that_role_now() {
    let d = tempfile::tempdir().unwrap();
    let home = home_with_config(d.path());
    let repo = repo_of(&[
        ("h1.json", handler_json("h1", r#"["overmind"]"#, "null")),
        (
            "h2.json",
            handler_json("h2", r#"["overmind"]"#, r#""2020-01-01T00:00:00Z""#),
        ),
    ]);
    let say = |role: &str| {
        let mut c = ctx(d.path(), Some(home.clone()), repo_of_clone(&repo));
        assert!(ok(run(&mut c, Some(role.to_string()))));
        c.captured().0
    };
    let overmind = say("overmind");
    assert!(
        overmind.contains("  active h1 ") && overmind.contains("[applies to overmind]"),
        "{overmind}"
    );
    // h2 is expired: listed, and NOT active for the role it names.
    let h2 = overmind
        .lines()
        .find(|l| l.contains("active h2 "))
        .expect(&overmind);
    assert!(
        h2.ends_with("expires=2020-01-01T00:00:00+00:00 [NOT active for overmind]"),
        "{h2}"
    );
    // The control: a role no approval names is not applied to by the live one either.
    let other = say("baracuda");
    assert!(!other.contains("[applies to"), "{other}");
    assert!(other.contains("[NOT active for baracuda]"), "{other}");
}

fn repo_of_clone(r: &FakeRepo) -> FakeRepo {
    FakeRepo(r.0.clone())
}

#[test]
fn a_refused_approval_is_named_with_its_reason_and_the_rest_still_load() {
    let d = tempfile::tempdir().unwrap();
    let home = home_with_config(d.path());
    let repo = repo_of(&[
        ("good.json", handler_json("good", r#"["overmind"]"#, "null")),
        // The file's name must be its handler's id.
        (
            "liar.json",
            handler_json("someone-else", r#"["overmind"]"#, "null"),
        ),
    ]);
    let mut ctx = ctx(d.path(), Some(home), repo);
    assert!(ok(run(&mut ctx, None)));
    let out = ctx.captured().0;
    assert!(out.starts_with("approvals: 1 active, 1 refused,"), "{out}");
    assert!(out.contains("  active good "), "{out}");
    assert!(out.contains("  REFUSED liar.json: "), "{out}");
}

#[test]
fn an_unreachable_repo_loads_nothing_and_fails() {
    let d = tempfile::tempdir().unwrap();
    let home = home_with_config(d.path());
    let mut ctx = ctx(d.path(), Some(home), Down);
    assert!(failed(run(&mut ctx, None)));
    let out = ctx.captured().0;
    assert!(out.starts_with("approvals: 0 active - could not resolve ciresnave/approvals's default branch: network is down"), "{out}");
}

#[test]
fn no_config_file_is_nothing_active_and_a_failure_that_says_where_it_looked() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("empty-home");
    std::fs::create_dir_all(&home).unwrap();
    let mut ctx = ctx(d.path(), Some(home), Down);
    assert!(failed(run(&mut ctx, None)));
    let out = ctx.captured().0;
    assert!(
        out.starts_with("approvals: 0 active - no approvals repo configured ("),
        "{out}"
    );
    assert!(out.contains("lane-restart.json does not exist"), "{out}");
}

#[test]
fn no_home_directory_is_named_on_stderr_and_fails_before_anything_is_read() {
    let d = tempfile::tempdir().unwrap();
    let mut ctx = ctx(d.path(), None, Down);
    assert!(failed(run(&mut ctx, None)));
    let (out, err) = ctx.captured();
    assert_eq!(out, "");
    assert_eq!(err, "lane-restart approvals: no home directory\n");
}

#[test]
fn the_command_line_reaches_it_through_run_with_and_a_bad_flag_does_not() {
    let d = tempfile::tempdir().unwrap();
    let home = home_with_config(d.path());
    let repo = repo_of(&[("h1.json", handler_json("h1", r#"["overmind"]"#, "null"))]);
    let mut ctx = ctx(d.path(), Some(home.clone()), repo);
    assert!(ok(run_with(
        &mut ctx,
        args(&["approvals", "--role", "overmind"])
    )));
    assert!(ctx.captured().0.contains("[applies to overmind]"));
    let mut bad = ctx_down(d.path(), home);
    assert!(failed(run_with(&mut bad, args(&["approvals", "--bogus"]))));
    let (out, err) = bad.captured();
    assert_eq!(out, "");
    assert!(err.starts_with("lane-restart approvals: "), "{err}");
}

fn ctx_down(root: &Path, home: PathBuf) -> Ctx {
    ctx(root, Some(home), Down)
}
