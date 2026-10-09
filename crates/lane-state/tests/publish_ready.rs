// SPDX-License-Identifier: MIT OR Apache-2.0
//! What must be true of this crate before it is published to crates.io, as a
//! test, so a later edit cannot quietly undo it.
//!
//! These tests pass on BOTH shapes of the manifest: the source one in the
//! repository (`license.workspace = true`, single-line arrays) and the one
//! cargo writes into the packaged crate (values resolved, `[dependencies.x]`
//! tables, multi-line arrays). Only the last test needs the workspace's root
//! manifest, and it skips itself when that is not there (the packaged crate).

use std::path::Path;

const MANIFEST: &str = include_str!("../Cargo.toml");
const README: &str = include_str!("../README.md");
const MIT: &str = include_str!("../LICENSE-MIT");
const APACHE: &str = include_str!("../LICENSE-APACHE");

/// The `[section]` tables of a manifest: (header without brackets, body).
fn sections(manifest: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in manifest.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') && !t.starts_with("[[") {
            out.push((
                t.trim_matches(|c| c == '[' || c == ']').to_string(),
                String::new(),
            ));
        } else if let Some(last) = out.last_mut() {
            last.1.push_str(line);
            last.1.push('\n');
        }
    }
    out
}

fn package_body() -> String {
    sections(MANIFEST)
        .into_iter()
        .find(|(h, _)| h == "package")
        .expect("a [package] table")
        .1
}

/// `key = "value"` in `[package]`.
fn package_value(key: &str) -> Option<String> {
    package_body()
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix(&format!("{key} = "))
                .map(str::to_string)
        })
        .filter(|v| v.starts_with('"'))
        .map(|v| v.trim().trim_matches('"').to_string())
}

/// `key = [ "a", "b" ]` in `[package]`, on one line or many.
fn package_list(key: &str) -> Vec<String> {
    let body = package_body();
    let start = body
        .find(&format!("{key} = ["))
        .unwrap_or_else(|| panic!("{key} missing"));
    let rest = &body[start + format!("{key} = [").len()..];
    rest[..rest.find(']').expect("a closing bracket")]
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Either the inherited form or the resolved value.
fn inherits_or_equals(key: &str, resolved_prefix: &str) -> bool {
    MANIFEST.contains(&format!("{key}.workspace = true"))
        || package_value(key).is_some_and(|v| v.starts_with(resolved_prefix))
}

#[test]
fn the_manifest_carries_what_crates_io_asks_for() {
    assert_eq!(package_value("name").as_deref(), Some("lane-state"));
    assert!(
        package_value("description").is_some_and(|d| d.len() > 40),
        "a real description"
    );
    assert_eq!(package_value("readme").as_deref(), Some("README.md"));
    assert!(inherits_or_equals("license", "MIT OR Apache-2.0"));
    assert!(inherits_or_equals(
        "repository",
        "https://github.com/ciresnave/OverMind"
    ));
    let rv = package_value("rust-version").expect("rust-version declared");
    assert!(rv.starts_with("1."), "{rv}");
}

#[test]
fn keywords_and_categories_fit_crates_io_limits() {
    let kw = package_list("keywords");
    assert!(
        (1..=5).contains(&kw.len()),
        "{kw:?}: crates.io allows at most 5"
    );
    assert!(kw.iter().all(|k| k.len() <= 20 && k.is_ascii()), "{kw:?}");
    let cats = package_list("categories");
    assert!(!cats.is_empty() && cats.len() <= 5, "{cats:?}");
}

/// Nothing in any dependency table may be a path or git dependency: the crate
/// must resolve from crates.io alone (a dev-dependency with a path and no
/// version breaks `cargo package` too).
#[test]
fn the_crate_depends_on_published_crates_only() {
    let dep_tables: Vec<(String, String)> = sections(MANIFEST)
        .into_iter()
        .filter(|(h, _)| {
            ["dependencies", "dev-dependencies", "build-dependencies"]
                .iter()
                .any(|k| h == k || h.starts_with(&format!("{k}.")))
        })
        .collect();
    assert!(
        dep_tables
            .iter()
            .any(|(h, _)| h.starts_with("dependencies")),
        "no dependency tables found: the parser is wrong"
    );
    for (header, body) in dep_tables {
        for line in body.lines() {
            assert!(
                !line.contains("path =") && !line.contains("git ="),
                "not publishable: [{header}] {line}"
            );
        }
    }
}

/// The README states the honest scope: what it is for, who it is for, and that
/// it is pre-1.0; and it does not claim the crate never stops a process.
#[test]
fn the_readme_states_the_scope_honestly() {
    for needle in [
        "lane-state",
        "Claude Code",
        "pre-1.0",
        "not a general-purpose",
        "MIT OR Apache-2.0",
        "kill_verified",
    ] {
        assert!(README.contains(needle), "README does not say {needle:?}");
    }
    assert!(
        !README.contains("Nothing here starts, restarts or stops a lane"),
        "kill_verified does kill a process"
    );
}

/// Both licence texts ship inside the crate (the dual licence is a choice of
/// either, and the tarball must carry both).
#[test]
fn both_licence_texts_are_in_the_crate() {
    assert!(MIT.contains("Permission is hereby granted, free of charge"));
    assert!(APACHE.contains("Apache License") && APACHE.contains("Version 2.0"));
}

/// The workspace dependency that dependents use carries a VERSION, and it is
/// this crate's own version: a forgotten edit at the next workspace bump would
/// publish dependents asking for a stale `lane-state`. Skips itself in the
/// packaged crate, which has no workspace root above it.
#[test]
fn the_workspace_pin_equals_the_workspace_version() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    let Ok(text) = std::fs::read_to_string(&root) else {
        eprintln!(
            "no workspace root at {}: skipped (packaged crate)",
            root.display()
        );
        return;
    };
    let table = |name: &str| {
        sections(&text)
            .into_iter()
            .find(|(h, _)| h == name)
            .unwrap_or_else(|| panic!("[{name}] missing from the workspace manifest"))
            .1
    };
    let version_of = |body: &str, prefix: &str| -> String {
        let line = body
            .lines()
            .find(|l| l.trim().starts_with(prefix))
            .unwrap_or_else(|| panic!("{prefix} missing"));
        let after = &line[line.find("version").expect("a version") + "version".len()..];
        after
            .split('"')
            .nth(1)
            .expect("a quoted version")
            .to_string()
    };
    let package = version_of(&table("workspace.package"), "version");
    let pin = version_of(&table("workspace.dependencies"), "lane-state");
    assert_eq!(
        pin, package,
        "bump [workspace.dependencies] lane-state with the workspace version"
    );
    assert!(
        table("workspace.dependencies").contains("path = \"crates/lane-state\""),
        "the path stays, for local builds"
    );
}
