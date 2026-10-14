// SPDX-License-Identifier: MIT OR Apache-2.0

//! Dogfood: this repository's own workflow and action files hold no mutable `uses:` ref (SUP-008),
//! judged by the same scanner `pin-plan` reports with. Files come from `git ls-files` (the index),
//! never a disk walk, and the text is read from the checkout.

use pin_plan::{scan, Kind};
use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

/// `(path, text)` of every workflow or action file in the index.
fn own_files() -> Vec<(String, String)> {
    let root = repo_root();
    let out = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["ls-files", "-z"])
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git ls-files failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("utf-8 paths")
        .split('\0')
        .filter(|p| {
            let l = p.to_ascii_lowercase();
            let name = l.rsplit('/').next().unwrap_or("");
            (l.ends_with(".yml") || l.ends_with(".yaml"))
                && (l.starts_with(".github/") || name == "action.yml" || name == "action.yaml")
        })
        .map(|p| {
            let text = std::fs::read_to_string(root.join(p)).unwrap_or_else(|e| panic!("{p}: {e}"));
            (p.to_string(), text)
        })
        .collect()
}

#[test]
fn this_repos_workflows_hold_no_mutable_ref() {
    let files = own_files();
    // Positive control: the enumeration finds the CI workflow and the scanner finds its refs, so
    // "none mutable" below is a verdict about real input and not about an empty one.
    assert!(
        files.iter().any(|(p, _)| p == ".github/workflows/ci.yml"),
        "ci.yml not enumerated; got {:?}",
        files.iter().map(|(p, _)| p).collect::<Vec<_>>()
    );
    let uses: Vec<_> = files
        .iter()
        .flat_map(|(p, t)| scan(p, t).into_iter().map(move |u| (u, t.clone())))
        .collect();
    let remote = uses
        .iter()
        .filter(|(u, _)| !matches!(u.kind, Kind::Local | Kind::Docker))
        .count();
    assert!(
        remote >= 3,
        "expected the CI actions to be found, saw {remote}"
    );

    let mutable: Vec<String> = uses
        .iter()
        .filter(|(u, _)| u.is_mutable())
        .map(|(u, _)| format!("{}:{} `{}`", u.file, u.line, u.raw))
        .collect();
    assert!(
        mutable.is_empty(),
        "mutable `uses:` refs (pin to the SHA the ref names and comment the tag): {mutable:#?}"
    );

    // "Comment the tag": every pinned ref names, after its SHA, the ref it stands for.
    let uncommented: Vec<String> = uses
        .iter()
        .filter(|(u, _)| u.kind == Kind::Pinned)
        .filter(|(u, text)| {
            let line = text.lines().nth(u.line - 1).unwrap_or("");
            line.split_once(&u.raw)
                .is_none_or(|(_, after)| !after.contains('#'))
        })
        .map(|(u, _)| format!("{}:{} `{}`", u.file, u.line, u.raw))
        .collect();
    assert!(
        uncommented.is_empty(),
        "pinned refs with no comment naming the tag: {uncommented:#?}"
    );
}
