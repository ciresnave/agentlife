// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart lane-new`, slice 2: the repo (step 1) and the clone + seed (step 2) - docs/LANE-NEW.md.
//!
//! `gh` and `git` sit behind [`Vcs`] so every step is tested against a fake; [`GhGit`] is the real one and
//! runs each tool with an argument array (never a shell), so no quoting or escaping is ever involved. The
//! seed is the PM's 2026-10-30 ruling: licences from onemkl (hash-checked), `/target`, a README that says
//! planned, capant's CI shape, and a **stub crate** (`publish = false`, version 0.0.0) so the required checks
//! run for real from the first push instead of checking nothing.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// SHA-256 of onemkl's `LICENSE-MIT` and `LICENSE-APACHE` at its `origin/main` (read 2026-10-30,
/// holder Eric Evans: "correct until the entities exist"). The embedded copies must hash to these.
pub const LICENSE_MIT_SHA256: &str =
    "b92c79e2189193acd8e0d7644c5af65f55bf1924ba0b41163529ca7440b5aa85";
pub const LICENSE_APACHE_SHA256: &str =
    "eb6cb356f35e63dddebf36659842103b68458610f79a677fc0b577e1cd0c2995";

/// Status contexts a protection rule should require (the CI template's job names). `portfolio` is
/// advisory (CireSnave 2026-10-23) and is not listed.
pub const REQUIRED_CONTEXTS: [&str; 7] = [
    "fmt",
    "clippy",
    "test (ubuntu-latest)",
    "test (windows-latest)",
    "test (macos-latest)",
    "doc",
    "cargo deny (advisories, licences, bans, sources)",
];

struct Template {
    path: &'static str,
    body: &'static str,
}

const TEMPLATES: [Template; 12] = [
    Template {
        path: "README.md",
        body: include_str!("../templates/lane-new/README.md.tmpl"),
    },
    Template {
        path: "HANDOFF.md",
        body: include_str!("../templates/lane-new/HANDOFF.md.tmpl"),
    },
    Template {
        path: ".gitignore",
        body: include_str!("../templates/lane-new/gitignore.tmpl"),
    },
    Template {
        path: ".gitattributes",
        body: include_str!("../templates/lane-new/gitattributes.tmpl"),
    },
    Template {
        path: "LICENSE-MIT",
        body: include_str!("../templates/lane-new/LICENSE-MIT"),
    },
    Template {
        path: "LICENSE-APACHE",
        body: include_str!("../templates/lane-new/LICENSE-APACHE"),
    },
    Template {
        path: "Cargo.toml",
        body: include_str!("../templates/lane-new/Cargo.toml.tmpl"),
    },
    Template {
        path: "Cargo.lock",
        body: include_str!("../templates/lane-new/Cargo.lock.tmpl"),
    },
    Template {
        path: "src/lib.rs",
        body: include_str!("../templates/lane-new/lib.rs.tmpl"),
    },
    Template {
        path: "deny.toml",
        body: include_str!("../templates/lane-new/deny.toml"),
    },
    Template {
        path: ".github/workflows/ci.yml",
        body: include_str!("../templates/lane-new/github-ci.yml.tmpl"),
    },
    Template {
        path: ".github/CODEOWNERS",
        body: include_str!("../templates/lane-new/github-CODEOWNERS.tmpl"),
    },
];

/// One file of the seed, rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedFile {
    pub path: &'static str,
    pub bytes: Vec<u8>,
}

/// A name `cargo` accepts as a package name (the stub crate takes the repo's name).
pub fn cargo_name_ok(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next().is_some_and(|c| c.is_ascii_alphabetic())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The seed for `role` / `org`/`repo`. `Err` when the repo is not a cargo name or a placeholder is left over.
pub fn render(org: &str, role: &str, repo: &str) -> Result<Vec<SeedFile>, String> {
    if !cargo_name_ok(repo) {
        return Err(format!(
            "repo {repo:?} is not a cargo package name (letters, digits, - and _, starting with a letter), which the stub crate needs"
        ));
    }
    let mut out = Vec::new();
    for t in &TEMPLATES {
        let body = t
            .body
            .replace("{{org}}", org)
            .replace("{{role}}", role)
            .replace("{{repo}}", repo);
        if body.contains("{{") && t.path != ".github/workflows/ci.yml" {
            return Err(format!("{}: an unreplaced placeholder is left", t.path));
        }
        out.push(SeedFile {
            path: t.path,
            bytes: body.into_bytes(),
        });
    }
    Ok(out)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoState {
    Absent,
    Exists { private: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalState {
    /// No directory, or an empty one (a clone may go there).
    Absent,
    /// A directory that is not a git checkout and not empty.
    NotRepo,
    Repo {
        origin: String,
        /// `None` for an unborn branch.
        head: Option<String>,
        clean: bool,
    },
}

/// Everything `gh` and `git` do for steps 1 and 2. Errors are short strings with no secrets.
pub trait Vcs {
    fn repo_state(&self, nwo: &str) -> Result<RepoState, String>;
    fn create_public(&self, nwo: &str, description: &str) -> Result<(), String>;
    /// The commit `main` points at on the remote, `None` for an empty repository.
    fn remote_head(&self, nwo: &str) -> Result<Option<String>, String>;
    /// The file's bytes at that commit on the remote, `None` when it is not there.
    fn remote_file(&self, nwo: &str, sha: &str, path: &str) -> Result<Option<Vec<u8>>, String>;
    /// Every file path in the tree at that commit on the remote (no directories). `Err` if the listing is truncated.
    fn remote_paths(&self, nwo: &str, sha: &str) -> Result<Vec<String>, String>;
    /// Every file in the checkout that `git add -A` would commit (tracked, or untracked and not ignored).
    fn local_paths(&self, dir: &Path) -> Result<Vec<String>, String>;
    fn clone_repo(&self, nwo: &str, dir: &Path) -> Result<(), String>;
    fn local_state(&self, dir: &Path) -> Result<LocalState, String>;
    /// Stages everything, commits on `main`, returns the new commit.
    fn commit_all(&self, dir: &Path, message: &str) -> Result<String, String>;
    fn push_main(&self, dir: &Path) -> Result<(), String>;
}

/// The real `gh` + `git`.
pub struct GhGit;

fn run(cmd: &str, args: &[&str], dir: Option<&Path>) -> Result<std::process::Output, String> {
    let mut c = std::process::Command::new(cmd);
    c.args(args);
    if let Some(d) = dir {
        c.current_dir(d);
    }
    c.output().map_err(|e| format!("cannot run {cmd}: {e}"))
}

fn text(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn short_err(o: &std::process::Output) -> String {
    let s = String::from_utf8_lossy(&o.stderr).trim().to_string();
    s.chars().take(300).collect()
}

fn url(nwo: &str) -> String {
    format!("https://github.com/{nwo}.git")
}

impl Vcs for GhGit {
    fn repo_state(&self, nwo: &str) -> Result<RepoState, String> {
        let o = run(
            "gh",
            &["api", &format!("repos/{nwo}"), "--jq", ".private"],
            None,
        )?;
        if o.status.success() {
            return match text(&o).as_str() {
                "true" => Ok(RepoState::Exists { private: true }),
                "false" => Ok(RepoState::Exists { private: false }),
                other => Err(format!("cannot read visibility of {nwo}: {other:?}")),
            };
        }
        let e = short_err(&o);
        // A private repo the bot cannot see reads as 404 too; creating it then fails loudly.
        if e.contains("HTTP 404") || e.contains("Not Found") {
            Ok(RepoState::Absent)
        } else {
            Err(format!("gh api repos/{nwo}: {e}"))
        }
    }

    fn create_public(&self, nwo: &str, description: &str) -> Result<(), String> {
        let o = run(
            "gh",
            &[
                "repo",
                "create",
                nwo,
                "--public",
                "--description",
                description,
            ],
            None,
        )?;
        if o.status.success() {
            Ok(())
        } else {
            Err(format!("gh repo create {nwo}: {}", short_err(&o)))
        }
    }

    fn remote_head(&self, nwo: &str) -> Result<Option<String>, String> {
        let o = run("git", &["ls-remote", &url(nwo), "refs/heads/main"], None)?;
        if !o.status.success() {
            return Err(format!("git ls-remote {nwo}: {}", short_err(&o)));
        }
        Ok(text(&o).split_whitespace().next().map(str::to_string))
    }

    fn remote_file(&self, nwo: &str, sha: &str, path: &str) -> Result<Option<Vec<u8>>, String> {
        let o = run(
            "gh",
            &[
                "api",
                &format!("repos/{nwo}/contents/{path}?ref={sha}"),
                "-H",
                "Accept: application/vnd.github.raw",
            ],
            None,
        )?;
        if o.status.success() {
            return Ok(Some(o.stdout));
        }
        let e = short_err(&o);
        if e.contains("HTTP 404") || e.contains("Not Found") {
            Ok(None)
        } else {
            Err(format!("gh api contents/{path}: {e}"))
        }
    }

    fn remote_paths(&self, nwo: &str, sha: &str) -> Result<Vec<String>, String> {
        let o = run(
            "gh",
            &[
                "api",
                &format!("repos/{nwo}/git/trees/{sha}?recursive=1"),
                "--jq",
                r#"if .truncated then error("truncated") else (.tree[] | select(.type != "tree") | .path) end"#,
            ],
            None,
        )?;
        if o.status.success() {
            Ok(text(&o).lines().map(str::to_string).collect())
        } else {
            Err(format!("gh api git/trees: {}", short_err(&o)))
        }
    }

    fn local_paths(&self, dir: &Path) -> Result<Vec<String>, String> {
        let o = run(
            "git",
            &["ls-files", "--cached", "--others", "--exclude-standard"],
            Some(dir),
        )?;
        if o.status.success() {
            Ok(text(&o).lines().map(str::to_string).collect())
        } else {
            Err(format!("git ls-files: {}", short_err(&o)))
        }
    }

    fn clone_repo(&self, nwo: &str, dir: &Path) -> Result<(), String> {
        let d = dir.to_string_lossy();
        let o = run("git", &["clone", &url(nwo), &d], None)?;
        if o.status.success() {
            Ok(())
        } else {
            Err(format!("git clone {nwo}: {}", short_err(&o)))
        }
    }

    fn local_state(&self, dir: &Path) -> Result<LocalState, String> {
        if !dir.exists() {
            return Ok(LocalState::Absent);
        }
        if !dir.join(".git").exists() {
            let empty = std::fs::read_dir(dir)
                .map_err(|e| e.to_string())?
                .next()
                .is_none();
            return Ok(if empty {
                LocalState::Absent
            } else {
                LocalState::NotRepo
            });
        }
        let origin = run("git", &["remote", "get-url", "origin"], Some(dir))?;
        let origin = if origin.status.success() {
            text(&origin)
        } else {
            String::new()
        };
        let head = run("git", &["rev-parse", "--verify", "-q", "HEAD"], Some(dir))?;
        let head = head.status.success().then(|| text(&head));
        let st = run("git", &["status", "--porcelain"], Some(dir))?;
        if !st.status.success() {
            return Err(format!("git status: {}", short_err(&st)));
        }
        Ok(LocalState::Repo {
            origin,
            head,
            clean: text(&st).is_empty(),
        })
    }

    fn commit_all(&self, dir: &Path, message: &str) -> Result<String, String> {
        for args in [&["checkout", "-B", "main"][..], &["add", "-A"][..]] {
            let o = run("git", args, Some(dir))?;
            if !o.status.success() {
                return Err(format!("git {}: {}", args[0], short_err(&o)));
            }
        }
        let o = run("git", &["commit", "-m", message], Some(dir))?;
        if !o.status.success() {
            return Err(format!("git commit: {}", short_err(&o)));
        }
        let h = run("git", &["rev-parse", "HEAD"], Some(dir))?;
        if h.status.success() {
            Ok(text(&h))
        } else {
            Err(format!("git rev-parse HEAD: {}", short_err(&h)))
        }
    }

    fn push_main(&self, dir: &Path) -> Result<(), String> {
        let o = run(
            "git",
            &["push", "origin", "HEAD:refs/heads/main"],
            Some(dir),
        )?;
        if o.status.success() {
            Ok(())
        } else {
            Err(format!("git push: {}", short_err(&o)))
        }
    }
}

/// What step 1 or 2 did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Already,
    Would,
}

fn origin_matches(origin: &str, nwo: &str) -> bool {
    let o = origin
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .to_ascii_lowercase();
    o == format!("https://github.com/{nwo}").to_ascii_lowercase()
        || o == format!("git@github.com:{nwo}").to_ascii_lowercase()
}

/// Problems with what already exists at the clone directory, for step 0 (nothing is written).
pub fn local_problems(vcs: &dyn Vcs, dir: &Path, nwo: &str) -> Vec<String> {
    match vcs.local_state(dir) {
        Err(e) => vec![format!("cannot read {}: {e}", dir.display())],
        Ok(LocalState::Absent) => vec![],
        Ok(LocalState::NotRepo) => {
            vec![format!(
                "{} exists, is not empty and is not a git checkout",
                dir.display()
            )]
        }
        Ok(LocalState::Repo { origin, .. }) if !origin_matches(&origin, nwo) => {
            vec![format!("{} is a checkout of another remote", dir.display())]
        }
        Ok(LocalState::Repo { .. }) => vec![],
    }
}

/// Step 1: the repo exists, public. Creates it only when `apply`.
pub fn step_repo(vcs: &dyn Vcs, nwo: &str, apply: bool) -> Result<(Outcome, String), String> {
    match vcs.repo_state(nwo)? {
        RepoState::Exists { private: false } => {
            Ok((Outcome::Already, format!("{nwo} exists, public")))
        }
        RepoState::Exists { private: true } => Err(format!(
            "{nwo} exists and is private; lane repos are public"
        )),
        RepoState::Absent if !apply => {
            Ok((Outcome::Would, format!("create {nwo} (public, bot token)")))
        }
        RepoState::Absent => {
            vcs.create_public(
                nwo,
                "Planned. A ThinkersJournal project; the crate name is reserved.",
            )?;
            match vcs.repo_state(nwo)? {
                RepoState::Exists { private: false } => {
                    Ok((Outcome::Ok, format!("created {nwo}; read back: public")))
                }
                other => Err(format!("created {nwo} but the read back says {other:?}")),
            }
        }
    }
}

/// The paths that are not a seed file's, sorted.
fn not_in_seed(paths: Vec<String>, seed: &[SeedFile]) -> Vec<String> {
    let mut v: Vec<String> = paths
        .into_iter()
        .filter(|p| !seed.iter().any(|f| f.path == p))
        .collect();
    v.sort();
    v
}

/// At most three names, and how many more.
fn list_some(v: &[String]) -> String {
    let shown: Vec<&str> = v.iter().take(3).map(String::as_str).collect();
    if v.len() > 3 {
        format!("{} and {} more", shown.join(", "), v.len() - 3)
    } else {
        shown.join(", ")
    }
}

fn read_local(dir: &Path, path: &str) -> Option<Vec<u8>> {
    std::fs::read(dir.join(path)).ok()
}

/// Step 2: clone, write the missing seed files, commit, push, read back. Never overwrites: a file that
/// exists and differs (locally, or on the remote's `main`) fails the step. `repo_exists` is false only in a
/// dry run where step 1 would create the repo.
pub fn step_seed(
    vcs: &dyn Vcs,
    nwo: &str,
    dir: &Path,
    seed: &[SeedFile],
    apply: bool,
    repo_exists: bool,
) -> Result<(Outcome, String), String> {
    // --- what the remote has ---
    let head = if repo_exists {
        vcs.remote_head(nwo)?
    } else {
        None
    };
    // An existing public repo is used only if it is empty or holds nothing but seed files (the resume case
    // after a failed step 7). Anything else in it is somebody's work: refuse, push nothing.
    if let Some(sha) = &head {
        let extra = not_in_seed(vcs.remote_paths(nwo, sha)?, seed);
        if !extra.is_empty() {
            return Err(format!(
                "{nwo} is not empty and not seed-only: its main also holds {}; lane-new only uses an empty repo or one holding the seed",
                list_some(&extra)
            ));
        }
    }
    let mut remote_missing = Vec::new();
    let mut remote_differs = Vec::new();
    for f in seed {
        match &head {
            None => remote_missing.push(f.path),
            Some(sha) => match vcs.remote_file(nwo, sha, f.path)? {
                None => remote_missing.push(f.path),
                Some(b) if b == f.bytes => {}
                Some(_) => remote_differs.push(f.path),
            },
        }
    }
    if !remote_differs.is_empty() {
        return Err(format!(
            "main of {nwo} already has different content for {}: not overwriting",
            remote_differs.join(", ")
        ));
    }
    // --- what the directory has ---
    let state = vcs.local_state(dir)?;
    match &state {
        LocalState::NotRepo => return Err(format!("{} is not a git checkout", dir.display())),
        LocalState::Repo { origin, .. } if !origin_matches(origin, nwo) => {
            return Err(format!("{} is a checkout of another remote", dir.display()))
        }
        _ => {}
    }
    // `commit_all` runs `git add -A`: a stray file in an existing checkout would be pushed to a public repo.
    if matches!(state, LocalState::Repo { .. }) {
        let extra = not_in_seed(vcs.local_paths(dir)?, seed);
        if !extra.is_empty() {
            return Err(format!(
                "{} holds files that are not part of the seed ({}): nothing is committed or pushed",
                dir.display(),
                list_some(&extra)
            ));
        }
    }
    let local_clean_at_head = matches!(&state, LocalState::Repo { head: Some(h), clean: true, .. } if Some(h) == head.as_ref());
    if remote_missing.is_empty() && local_clean_at_head {
        return Ok((
            Outcome::Already,
            format!(
                "{} at {}, seed complete",
                dir.display(),
                head.unwrap_or_default()
            ),
        ));
    }
    if !apply {
        return Ok((
            Outcome::Would,
            format!(
                "{} {}, write {} file(s), commit, push main",
                if matches!(state, LocalState::Absent) {
                    "clone to"
                } else {
                    "use"
                },
                dir.display(),
                remote_missing.len()
            ),
        ));
    }
    // --- act ---
    if matches!(state, LocalState::Absent) {
        vcs.clone_repo(nwo, dir)?;
    }
    let mut wrote = 0;
    for f in seed {
        match read_local(dir, f.path) {
            Some(b) if b == f.bytes => {}
            Some(_) => {
                return Err(format!(
                    "{} exists in the checkout and differs: not overwriting",
                    f.path
                ))
            }
            None if !remote_missing.contains(&f.path) => {
                return Err(format!(
                    "{} is on the remote but missing in the checkout: pull it first",
                    f.path
                ))
            }
            None => {
                let p: PathBuf = dir.join(f.path);
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
                }
                std::fs::write(&p, &f.bytes)
                    .map_err(|e| format!("cannot write {}: {e}", p.display()))?;
                wrote += 1;
            }
        }
    }
    let local_head = match vcs.local_state(dir)? {
        LocalState::Repo {
            clean: true,
            head: Some(h),
            ..
        } => h,
        LocalState::Repo { .. } => vcs.commit_all(
            dir,
            &format!("seed: {nwo} lane scaffold (lane-restart lane-new)"),
        )?,
        other => return Err(format!("after cloning, {} is {other:?}", dir.display())),
    };
    if Some(&local_head) != head.as_ref() {
        vcs.push_main(dir)?;
    }
    // --- read back: the remote's main is what we have, and every seed file is there byte for byte ---
    let after = vcs.remote_head(nwo)?;
    if after.as_deref() != Some(local_head.as_str()) {
        return Err(format!(
            "pushed {local_head} but the remote's main reads {after:?}"
        ));
    }
    for f in seed {
        match vcs.remote_file(nwo, &local_head, f.path)? {
            Some(b) if b == f.bytes => {}
            Some(_) => return Err(format!("{} on the remote differs from the seed", f.path)),
            None => {
                return Err(format!(
                    "{} is missing on the remote after the push",
                    f.path
                ))
            }
        }
    }
    Ok((
        Outcome::Ok,
        format!(
            "{} at {local_head}; {wrote} file(s) written, {} on the remote",
            dir.display(),
            seed.len()
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::{BTreeMap, HashMap};
    use tempfile::tempdir;

    const NWO: &str = "ThinkersJournal/zetafish";

    /// The files of a remote's `main` by path, and the commit they are at.
    type Tree = BTreeMap<String, Vec<u8>>;

    /// An in-memory forge plus a pretend git whose state lives in `<dir>/.fakegit`.
    #[derive(Default)]
    struct Fake {
        repos: RefCell<HashMap<String, bool>>,
        remote: RefCell<HashMap<String, (String, Tree)>>,
        calls: RefCell<Vec<String>>,
        dirs: RefCell<Vec<PathBuf>>,
        lie_push: bool,
        lie_create: bool,
    }

    fn tree_of(dir: &Path) -> BTreeMap<String, Vec<u8>> {
        fn walk(base: &Path, d: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            for e in std::fs::read_dir(d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(base, &p, out);
                } else if p.file_name().unwrap() != ".fakegit" {
                    let rel = p
                        .strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/");
                    out.insert(rel, std::fs::read(&p).unwrap());
                }
            }
        }
        let mut m = BTreeMap::new();
        walk(dir, dir, &mut m);
        m
    }

    fn id_of(t: &BTreeMap<String, Vec<u8>>) -> String {
        let mut h = Sha256::new();
        for (k, v) in t {
            h.update(k.as_bytes());
            h.update(v);
        }
        format!(
            "c{}",
            &h.finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()[..12]
        )
    }

    impl Fake {
        fn note(&self, s: &str) {
            self.calls.borrow_mut().push(s.to_string());
        }
        fn with_repo(self, nwo: &str) -> Fake {
            self.repos.borrow_mut().insert(nwo.to_string(), false);
            self
        }
        fn read_fg(dir: &Path) -> HashMap<String, String> {
            std::fs::read_to_string(dir.join(".fakegit"))
                .unwrap_or_default()
                .lines()
                .filter_map(|l| {
                    l.split_once('=')
                        .map(|(a, b)| (a.to_string(), b.to_string()))
                })
                .collect()
        }
        fn write_fg(dir: &Path, origin: &str, head: &str, tree: &str) {
            std::fs::write(
                dir.join(".fakegit"),
                format!("origin={origin}\nhead={head}\ntree={tree}\n"),
            )
            .unwrap();
        }
    }

    impl Vcs for Fake {
        fn repo_state(&self, nwo: &str) -> Result<RepoState, String> {
            self.note("repo_state");
            let hide = self.lie_create && self.calls.borrow().iter().any(|c| c == "create");
            Ok(match self.repos.borrow().get(nwo) {
                Some(&private) if !hide => RepoState::Exists { private },
                _ => RepoState::Absent,
            })
        }
        fn create_public(&self, nwo: &str, _d: &str) -> Result<(), String> {
            self.note("create");
            self.repos.borrow_mut().insert(nwo.to_string(), false);
            Ok(())
        }
        fn remote_head(&self, nwo: &str) -> Result<Option<String>, String> {
            Ok(self.remote.borrow().get(nwo).map(|(h, _)| h.clone()))
        }
        fn remote_file(&self, nwo: &str, sha: &str, path: &str) -> Result<Option<Vec<u8>>, String> {
            Ok(self
                .remote
                .borrow()
                .get(nwo)
                .filter(|(h, _)| h == sha)
                .and_then(|(_, f)| f.get(path).cloned()))
        }
        fn remote_paths(&self, nwo: &str, sha: &str) -> Result<Vec<String>, String> {
            Ok(self
                .remote
                .borrow()
                .get(nwo)
                .filter(|(h, _)| h == sha)
                .map(|(_, f)| f.keys().cloned().collect())
                .unwrap_or_default())
        }
        fn local_paths(&self, dir: &Path) -> Result<Vec<String>, String> {
            Ok(tree_of(dir).into_keys().collect())
        }
        fn clone_repo(&self, nwo: &str, dir: &Path) -> Result<(), String> {
            self.note("clone");
            self.dirs.borrow_mut().push(dir.to_path_buf());
            std::fs::create_dir_all(dir).unwrap();
            let (head, files) = self.remote.borrow().get(nwo).cloned().unwrap_or_default();
            for (k, v) in &files {
                let p = dir.join(k);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, v).unwrap();
            }
            let t = id_of(&files);
            Fake::write_fg(dir, &format!("https://github.com/{nwo}.git"), &head, &t);
            Ok(())
        }
        fn local_state(&self, dir: &Path) -> Result<LocalState, String> {
            self.dirs.borrow_mut().push(dir.to_path_buf());
            if !dir.exists() {
                return Ok(LocalState::Absent);
            }
            if !dir.join(".fakegit").exists() {
                let empty = std::fs::read_dir(dir).unwrap().next().is_none();
                return Ok(if empty {
                    LocalState::Absent
                } else {
                    LocalState::NotRepo
                });
            }
            let fg = Fake::read_fg(dir);
            let head = fg.get("head").filter(|h| !h.is_empty()).cloned();
            let clean = fg.get("tree") == Some(&id_of(&tree_of(dir)));
            Ok(LocalState::Repo {
                origin: fg["origin"].clone(),
                head,
                clean,
            })
        }
        fn commit_all(&self, dir: &Path, _m: &str) -> Result<String, String> {
            self.note("commit");
            let t = tree_of(dir);
            let id = id_of(&t);
            let origin = Fake::read_fg(dir)
                .get("origin")
                .cloned()
                .unwrap_or_default();
            Fake::write_fg(dir, &origin, &id, &id);
            Ok(id)
        }
        fn push_main(&self, dir: &Path) -> Result<(), String> {
            self.note("push");
            if self.lie_push {
                return Ok(());
            }
            let fg = Fake::read_fg(dir);
            let nwo = fg["origin"]
                .trim_start_matches("https://github.com/")
                .trim_end_matches(".git")
                .to_string();
            self.remote
                .borrow_mut()
                .insert(nwo, (fg["head"].clone(), tree_of(dir)));
            Ok(())
        }
    }

    fn seed() -> Vec<SeedFile> {
        render("ThinkersJournal", "zetafish", "zetafish").unwrap()
    }

    #[test]
    fn the_licences_are_onemkls_byte_for_byte_and_the_ci_has_the_required_jobs_unguarded() {
        let s = seed();
        let get = |p: &str| s.iter().find(|f| f.path == p).unwrap();
        assert_eq!(sha256_hex(&get("LICENSE-MIT").bytes), LICENSE_MIT_SHA256);
        assert_eq!(
            sha256_hex(&get("LICENSE-APACHE").bytes),
            LICENSE_APACHE_SHA256
        );
        // control: a changed byte no longer matches
        let mut bad = get("LICENSE-MIT").bytes.clone();
        bad[0] ^= 1;
        assert_ne!(sha256_hex(&bad), LICENSE_MIT_SHA256);
        let ci = String::from_utf8(get(".github/workflows/ci.yml").bytes.clone()).unwrap();
        for name in [
            "name: fmt",
            "name: clippy",
            "name: test (${{ matrix.os }})",
            "name: doc",
            "name: cargo deny (advisories, licences, bans, sources)",
        ] {
            assert!(ci.contains(name), "{name}");
        }
        for os in ["ubuntu-latest", "windows-latest", "macos-latest"] {
            assert!(ci.contains(os), "{os}");
        }
        assert!(
            !ci.contains("hashFiles"),
            "a guarded job checks nothing until a Cargo.toml exists"
        );
        assert!(
            !ci.to_ascii_lowercase().contains("capant"),
            "capant-check is capant-specific"
        );
        for c in REQUIRED_CONTEXTS {
            assert!(!c.is_empty());
        }
    }

    #[test]
    fn the_stub_crate_is_publish_false_zero_zero_zero_and_its_lock_agrees_with_cargo() {
        let d = tempdir().unwrap();
        for f in seed() {
            let p = d.path().join(f.path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, &f.bytes).unwrap();
        }
        let toml = std::fs::read_to_string(d.path().join("Cargo.toml")).unwrap();
        assert!(
            toml.contains("publish = false") && toml.contains("version = \"0.0.0\""),
            "{toml}"
        );
        // `cargo metadata --locked` fails if Cargo.lock would need to change: the lock is what cargo writes.
        let o =
            std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
                .args(["metadata", "--locked", "--offline", "--format-version", "1"])
                .current_dir(d.path())
                .output()
                .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        // control: a lock with another version would be rejected
        std::fs::write(
            d.path().join("Cargo.lock"),
            String::from_utf8(
                seed()
                    .iter()
                    .find(|f| f.path == "Cargo.lock")
                    .unwrap()
                    .bytes
                    .clone(),
            )
            .unwrap()
            .replace("0.0.0", "9.9.9"),
        )
        .unwrap();
        let o =
            std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
                .args(["metadata", "--locked", "--offline", "--format-version", "1"])
                .current_dir(d.path())
                .output()
                .unwrap();
        assert!(
            !o.status.success(),
            "a wrong lock was accepted, so the check above proves nothing"
        );
    }

    #[test]
    fn a_repo_name_cargo_rejects_and_a_leftover_placeholder_are_refused() {
        assert!(render("o", "r", "9lives").is_err());
        assert!(render("o", "r", "a.b").is_err());
        assert!(render("o", "r", "a b").is_err());
        assert!(render("o", "r", "open-clown_fish2").is_ok());
        assert!(cargo_name_ok("zetafish") && !cargo_name_ok("-x"));
    }

    #[test]
    fn step_1_creates_once_reads_back_and_a_second_run_is_already() {
        let v = Fake::default();
        assert_eq!(step_repo(&v, NWO, false).unwrap().0, Outcome::Would);
        assert!(
            !v.calls.borrow().iter().any(|c| c == "create"),
            "a dry run created a repo"
        );
        assert_eq!(step_repo(&v, NWO, true).unwrap().0, Outcome::Ok);
        assert_eq!(step_repo(&v, NWO, true).unwrap().0, Outcome::Already);
        assert_eq!(
            v.calls.borrow().iter().filter(|c| *c == "create").count(),
            1
        );
    }

    #[test]
    fn step_1_refuses_a_private_repo_and_a_create_that_does_not_read_back() {
        let v = Fake::default();
        v.repos.borrow_mut().insert(NWO.into(), true);
        assert!(step_repo(&v, NWO, true).unwrap_err().contains("private"));
        let lie = Fake {
            lie_create: true,
            ..Fake::default()
        };
        assert!(step_repo(&lie, NWO, true)
            .unwrap_err()
            .contains("read back"));
    }

    #[test]
    fn step_2_seeds_an_empty_repo_pushes_and_reads_back_every_file() {
        let d = tempdir().unwrap();
        let dir = d.path().join("zetafish");
        let v = Fake::default().with_repo(NWO);
        let (o, msg) = step_seed(&v, NWO, &dir, &seed(), true, true).unwrap();
        assert_eq!(o, Outcome::Ok, "{msg}");
        let remote = v.remote.borrow();
        let (_, files) = remote.get(NWO).unwrap();
        assert_eq!(files.len(), seed().len());
        for f in seed() {
            assert_eq!(files.get(f.path), Some(&f.bytes), "{}", f.path);
        }
        drop(remote);
        // every directory handed to git is absolute (the cwd incident)
        assert!(
            v.dirs.borrow().iter().all(|p| p.is_absolute()),
            "{:?}",
            v.dirs.borrow()
        );
        // a second run does nothing: no clone, commit or push
        let before = v.calls.borrow().len();
        let (o, _) = step_seed(&v, NWO, &dir, &seed(), true, true).unwrap();
        assert_eq!(o, Outcome::Already);
        assert!(
            v.calls.borrow()[before..].iter().all(|c| c == "repo_state"),
            "{:?}",
            &v.calls.borrow()[before..]
        );
    }

    #[test]
    fn a_push_that_did_not_land_is_a_failure_not_a_success() {
        let d = tempdir().unwrap();
        let v = Fake {
            lie_push: true,
            ..Fake::default()
        }
        .with_repo(NWO);
        let e = step_seed(&v, NWO, &d.path().join("zetafish"), &seed(), true, true).unwrap_err();
        assert!(e.contains("remote's main reads None"), "{e}");
        // control: the same run with an honest push succeeds
        let d2 = tempdir().unwrap();
        let ok = Fake::default().with_repo(NWO);
        assert_eq!(
            step_seed(&ok, NWO, &d2.path().join("zetafish"), &seed(), true, true)
                .unwrap()
                .0,
            Outcome::Ok
        );
    }

    #[test]
    fn a_dry_run_changes_nothing_and_says_what_it_would_do() {
        let d = tempdir().unwrap();
        let dir = d.path().join("zetafish");
        let v = Fake::default().with_repo(NWO);
        let (o, msg) = step_seed(&v, NWO, &dir, &seed(), false, true).unwrap();
        assert_eq!(o, Outcome::Would, "{msg}");
        assert!(!dir.exists(), "a dry run created the directory");
        assert!(v.remote.borrow().is_empty());
        assert!(!v
            .calls
            .borrow()
            .iter()
            .any(|c| c == "clone" || c == "commit" || c == "push"));
        // the repo does not exist yet either (step 1 would create it)
        let v2 = Fake::default();
        assert_eq!(
            step_seed(&v2, NWO, &dir, &seed(), false, false).unwrap().0,
            Outcome::Would
        );
    }

    #[test]
    fn existing_different_content_is_never_overwritten() {
        // on the remote
        let d = tempdir().unwrap();
        let v = Fake::default().with_repo(NWO);
        let mut other = BTreeMap::new();
        other.insert("README.md".to_string(), b"someone else's readme".to_vec());
        v.remote
            .borrow_mut()
            .insert(NWO.into(), (id_of(&other), other));
        let e = step_seed(&v, NWO, &d.path().join("zetafish"), &seed(), true, true).unwrap_err();
        assert!(
            e.contains("README.md") && e.contains("not overwriting"),
            "{e}"
        );
        assert!(
            !v.calls.borrow().iter().any(|c| c == "clone"),
            "it cloned before refusing"
        );
        // in the checkout
        let d2 = tempdir().unwrap();
        let dir = d2.path().join("zetafish");
        let v2 = Fake::default().with_repo(NWO);
        v2.clone_repo(NWO, &dir).unwrap();
        std::fs::write(dir.join("README.md"), b"mine").unwrap();
        let e = step_seed(&v2, NWO, &dir, &seed(), true, true).unwrap_err();
        assert!(e.contains("differs: not overwriting"), "{e}");
        assert_eq!(std::fs::read(dir.join("README.md")).unwrap(), b"mine");
    }

    fn remote_with(v: &Fake, files: &[(&str, &[u8])]) -> Tree {
        let t: Tree = files
            .iter()
            .map(|(k, b)| (k.to_string(), b.to_vec()))
            .collect();
        v.remote
            .borrow_mut()
            .insert(NWO.into(), (id_of(&t), t.clone()));
        t
    }

    fn pushed_or_committed(v: &Fake) -> bool {
        v.calls
            .borrow()
            .iter()
            .any(|c| c == "clone" || c == "commit" || c == "push")
    }

    #[test]
    fn an_existing_repo_with_an_unrelated_file_is_refused_and_nothing_is_pushed() {
        let s = seed();
        let readme = s.iter().find(|f| f.path == "README.md").unwrap();
        for apply in [false, true] {
            let d = tempdir().unwrap();
            let v = Fake::default().with_repo(NWO);
            let before = remote_with(
                &v,
                &[
                    ("README.md", readme.bytes.as_slice()),
                    ("notes/private.txt", b"somebody's work"),
                ],
            );
            let e = step_seed(&v, NWO, &d.path().join("zetafish"), &s, apply, true).unwrap_err();
            assert!(
                e.contains("not empty and not seed-only") && e.contains("notes/private.txt"),
                "apply={apply}: {e}"
            );
            assert!(!pushed_or_committed(&v), "apply={apply}: {:?}", v.calls);
            assert_eq!(v.remote.borrow()[NWO].1, before, "the remote changed");
            assert!(!d.path().join("zetafish").exists());
        }
    }

    #[test]
    fn an_existing_repo_holding_only_some_seed_files_is_the_resume_case_and_is_completed() {
        let s = seed();
        let readme = s.iter().find(|f| f.path == "README.md").unwrap();
        let d = tempdir().unwrap();
        let v = Fake::default().with_repo(NWO);
        remote_with(&v, &[("README.md", readme.bytes.as_slice())]);
        let (o, detail) = step_seed(&v, NWO, &d.path().join("zetafish"), &s, true, true).unwrap();
        assert_eq!(o, Outcome::Ok, "{detail}");
        let have: Vec<String> = v.remote.borrow()[NWO].1.keys().cloned().collect();
        let mut want: Vec<String> = s.iter().map(|f| f.path.to_string()).collect();
        want.sort();
        assert_eq!(
            have, want,
            "control: the seed is now complete and nothing else"
        );
    }

    #[test]
    fn a_checkout_holding_an_unrelated_file_is_refused_before_anything_is_committed() {
        let s = seed();
        let d = tempdir().unwrap();
        let dir = d.path().join("zetafish");
        let v = Fake::default().with_repo(NWO);
        let before = remote_with(&v, &[]);
        v.clone_repo(NWO, &dir).unwrap();
        std::fs::write(dir.join("api-key.txt"), b"hunter2").unwrap();
        v.calls.borrow_mut().clear();
        for apply in [false, true] {
            let e = step_seed(&v, NWO, &dir, &s, apply, true).unwrap_err();
            assert!(
                e.contains("not part of the seed") && e.contains("api-key.txt"),
                "apply={apply}: {e}"
            );
            assert!(!pushed_or_committed(&v), "apply={apply}: {:?}", v.calls);
        }
        assert_eq!(v.remote.borrow()[NWO].1, before);
        assert!(dir.join("api-key.txt").exists(), "the file was left alone");
    }

    #[test]
    fn a_checkout_of_another_remote_or_a_stray_directory_is_found_by_step_0() {
        let d = tempdir().unwrap();
        let v = Fake::default();
        assert!(local_problems(&v, &d.path().join("nope"), NWO).is_empty());
        let stray = d.path().join("stray");
        std::fs::create_dir_all(&stray).unwrap();
        std::fs::write(stray.join("x"), "x").unwrap();
        assert!(local_problems(&v, &stray, NWO)[0].contains("not a git checkout"));
        let other = d.path().join("other");
        v.clone_repo("ThinkersJournal/bonefish", &other).unwrap();
        assert!(local_problems(&v, &other, NWO)[0].contains("another remote"));
        let same = d.path().join("same");
        v.clone_repo(NWO, &same).unwrap();
        assert!(local_problems(&v, &same, NWO).is_empty());
    }

    #[test]
    fn a_commit_made_but_not_pushed_is_pushed_on_the_rerun() {
        let d = tempdir().unwrap();
        let dir = d.path().join("zetafish");
        let v = Fake {
            lie_push: true,
            ..Fake::default()
        }
        .with_repo(NWO);
        assert!(step_seed(&v, NWO, &dir, &seed(), true, true).is_err());
        // rerun against the same forge with an honest push: the commit exists locally, only the push is redone
        let honest = Fake::default().with_repo(NWO);
        let commits_before = v.calls.borrow().iter().filter(|c| *c == "commit").count();
        assert_eq!(commits_before, 1);
        let (o, msg) = step_seed(&honest, NWO, &dir, &seed(), true, true).unwrap();
        assert_eq!(o, Outcome::Ok, "{msg}");
        assert!(
            !honest
                .calls
                .borrow()
                .iter()
                .any(|c| c == "commit" || c == "clone"),
            "{:?}",
            honest.calls.borrow()
        );
    }

    // ---- the whole run (steps 0 to 5) against the fake -------------------------------------------

    use crate::lane_new::{run_with, Report, Roots, Status};
    use chrono::Utc;

    fn world(d: &tempfile::TempDir) -> Roots {
        let b = d.path();
        std::fs::create_dir_all(b.join("state")).unwrap();
        std::fs::write(b.join("state").join("roles-soak.txt"), "fuel\n").unwrap();
        Roots::new(
            b.join("state"),
            b.join("proj"),
            b.join("homes"),
            b.join("pilot"),
            b.join("bin").join("chan"),
        )
        .unwrap()
    }

    fn go(r: &Roots, v: &Fake, apply: bool) -> Report {
        std::fs::create_dir_all(&r.projects).unwrap();
        run_with(
            r,
            Some(v),
            "zetafish",
            "zetafish",
            "sonnet",
            apply,
            Utc::now(),
        )
    }

    fn st(rep: &Report) -> Vec<(u8, Status)> {
        rep.rows
            .iter()
            .filter(|x| x.n != 0 && x.status != Status::Later)
            .map(|x| (x.n, x.status))
            .collect()
    }

    #[test]
    fn a_full_run_creates_seeds_and_writes_the_lane_files_then_converges() {
        let d = tempdir().unwrap();
        let r = world(&d);
        let v = Fake::default();
        let rep = go(&r, &v, true);
        assert_eq!(
            st(&rep),
            vec![
                (1, Status::Ok),
                (2, Status::Ok),
                (3, Status::Ok),
                (4, Status::Ok),
                (5, Status::Ok)
            ],
            "{}",
            rep.render(true)
        );
        assert!(r.projects.join("zetafish").join("Cargo.toml").exists());
        assert!(r.state_dir.join("zetafish.launch.json").exists());
        let again = go(&r, &v, true);
        assert_eq!(
            st(&again),
            vec![
                (1, Status::Already),
                (2, Status::Already),
                (3, Status::Already),
                (4, Status::Already),
                (5, Status::Already)
            ],
            "{}",
            again.render(true)
        );
        assert_eq!(
            v.calls.borrow().iter().filter(|c| *c == "create").count(),
            1
        );
        assert!(v.dirs.borrow().iter().all(|p| p.is_absolute()));
        // the protection command is printed with the CI job names, not run
        let line = again.rows.iter().find(|x| x.n == 8).unwrap();
        assert!(
            line.detail
                .contains("protect-main.sh ThinkersJournal/zetafish")
                && line.detail.contains("\"test (windows-latest)\""),
            "{}",
            line.detail
        );
    }

    #[test]
    fn a_dry_run_only_reads_and_says_would_for_every_step() {
        let d = tempdir().unwrap();
        let r = world(&d);
        let v = Fake::default();
        let rep = go(&r, &v, false);
        assert_eq!(
            st(&rep),
            vec![
                (1, Status::Would),
                (2, Status::Would),
                (3, Status::Would),
                (4, Status::Would),
                (5, Status::Would)
            ],
            "{}",
            rep.render(false)
        );
        assert!(v.repos.borrow().is_empty() && v.remote.borrow().is_empty());
        assert!(!r.projects.join("zetafish").exists());
        assert!(!r.state_dir.join("zetafish.launch.json").exists());
        assert!(
            v.calls.borrow().iter().all(|c| c == "repo_state"),
            "{:?}",
            v.calls.borrow()
        );
    }

    #[test]
    fn a_failed_early_step_stops_the_run_before_any_lane_file_is_written() {
        let d = tempdir().unwrap();
        let r = world(&d);
        let v = Fake::default();
        v.repos.borrow_mut().insert(NWO.into(), true); // private
        let rep = go(&r, &v, true);
        assert!(
            rep.failed()
                && rep
                    .rows
                    .iter()
                    .any(|x| x.n == 1 && x.status == Status::Fail),
            "{}",
            rep.render(true)
        );
        assert!(
            !r.state_dir.join("zetafish.launch.json").exists()
                && !r.state_dir.join("zetafish.mcp.json").exists()
        );
        assert!(!v
            .calls
            .borrow()
            .iter()
            .any(|c| c == "create" || c == "clone" || c == "push"));
        // a checkout of another remote is found by step 0, before the repo is created
        let d2 = tempdir().unwrap();
        let r2 = world(&d2);
        let v2 = Fake::default();
        std::fs::create_dir_all(&r2.projects).unwrap();
        v2.clone_repo("ThinkersJournal/bonefish", &r2.projects.join("zetafish"))
            .unwrap();
        let rep = go(&r2, &v2, true);
        assert!(
            rep.failed() && rep.rows[0].detail.contains("another remote"),
            "{}",
            rep.render(true)
        );
        assert!(
            !v2.calls.borrow().iter().any(|c| c == "create"),
            "step 0 refused but a repo was created"
        );
    }

    #[test]
    fn without_a_vcs_steps_1_and_2_are_listed_as_later() {
        let d = tempdir().unwrap();
        let r = world(&d);
        let rep =
            crate::lane_new::run_local(&r, "zetafish", "zetafish", "sonnet", false, Utc::now());
        assert!(rep
            .rows
            .iter()
            .filter(|x| x.n == 1 || x.n == 2)
            .all(|x| x.status == Status::Later));
    }
}
