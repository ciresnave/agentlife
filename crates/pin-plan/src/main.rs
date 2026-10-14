// SPDX-License-Identifier: MIT OR Apache-2.0

//! `pin-plan` - SUP-008 planner. Read-only: for every repo under a root, every `uses:` in its
//! workflow and action files that names a mutable tag or branch, with the latest release tag's
//! commit SHA to pin it to and the Node runtime that action declares (node20 is flagged). Prints
//! one table per repo. It writes nothing: no file, no ref, no PR. The one exception is
//! `--emit-patch DIR`, which writes one `<repo>.patch` per repo into `DIR` and nowhere else.
//!
//! Files are enumerated with `git ls-files` (the index), never by walking a checkout, and their
//! text is read from `origin/main` (`git show`), falling back to `HEAD`, with the ref printed per
//! repo. Release data comes from `gh api`, once per action, however many files use it.

use pin_plan::{
    major_of, pick_latest_tag, pin_line_as, runs_using, runtime_needs_attention, scan,
    unified_diff, Kind, Use,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const USAGE: &str = "\
usage: pin-plan [--root DIR] [--repo NAME]... [--no-network] [--all] [--emit-patch DIR]
                [--target current|latest]

  --root DIR     the directory whose child repositories are planned (default C:/Projects)
  --repo NAME    only this child (repeatable)
  --no-network   do not call `gh`: the tables list the mutable refs only
  --all          also list refs that are already pinned (default: counted, not listed)
  --emit-patch DIR
                 write one unified diff per repo to DIR/<repo>.patch (`git apply` it in that
                 repo): each mutable ref becomes the SHA it names now, the old ref in a comment.
                 Nothing is written into a repo; a DIR inside one is refused. Needs the network.

  --target current|latest
                 what --emit-patch pins to (default current). `current`: the SHA the ref names now,
                 behaviour unchanged. `latest`: the SHA of the action's latest release tag (a tag
                 ref only; a branch ref is kept at the SHA it names now, because a branch such as
                 `dtolnay/rust-toolchain@stable` selects behaviour a release tag does not carry).
                 DIR/ACTIONS.md lists every action with its runtime now and at latest.

Read-only apart from --emit-patch. Exit 0 when the plan was produced, 1 when a repo could not be read, 2 for bad usage.
";

#[derive(Default)]
struct Args {
    root: Option<PathBuf>,
    repos: Vec<String>,
    no_network: bool,
    all: bool,
    emit: Option<PathBuf>,
    latest: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--root" => a.root = Some(it.next().ok_or("--root needs a value")?.into()),
            "--repo" => a.repos.push(it.next().ok_or("--repo needs a value")?),
            "--no-network" => a.no_network = true,
            "--all" => a.all = true,
            "--target" => {
                a.latest = match it.next().ok_or("--target needs a value")?.as_str() {
                    "current" => false,
                    "latest" => true,
                    other => return Err(format!("--target takes current or latest, not {other}")),
                }
            }
            "--emit-patch" => a.emit = Some(it.next().ok_or("--emit-patch needs a value")?.into()),
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(a)
}

fn run(cmd: &mut Command) -> Result<String, String> {
    let out = cmd.output().map_err(|e| format!("cannot run: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "exit {}: {}",
            out.status.code().map_or("?".into(), |c| c.to_string()),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    run(Command::new("git").arg("-C").arg(repo).args(args))
}

fn gh(args: &[&str]) -> Result<String, String> {
    run(Command::new("gh").args(args))
}

/// Child directories of `root` that are repositories (a `.git` directory). A `.git` *file* is a
/// linked worktree of another repo and is listed as skipped, not planned: its files are that
/// repo's files.
fn repos_under(root: &Path, only: &[String]) -> Result<(Vec<PathBuf>, Vec<String>), String> {
    let mut repos = Vec::new();
    let mut skipped = Vec::new();
    let rd = std::fs::read_dir(root).map_err(|e| format!("{}: {e}", root.display()))?;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !only.is_empty() && !only.contains(&name) {
            continue;
        }
        let p = e.path();
        let dotgit = p.join(".git");
        if dotgit.is_dir() {
            repos.push(p);
        } else if dotgit.is_file() {
            skipped.push(format!("{name} (linked worktree)"));
        }
    }
    repos.sort();
    skipped.sort();
    Ok((repos, skipped))
}

fn interesting(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let yaml = lower.ends_with(".yml") || lower.ends_with(".yaml");
    let name = lower.rsplit('/').next().unwrap_or("");
    yaml && (lower.starts_with(".github/") || name == "action.yml" || name == "action.yaml")
}

/// Everything known about a remote action, resolved once.
#[derive(Clone)]
struct Resolved {
    tag: String,
    sha: String,
    how: &'static str,
}

struct Remote {
    enabled: bool,
    latest: BTreeMap<(String, String), Result<Resolved, String>>,
    runtime: BTreeMap<(String, String, String, String), Result<String, String>>,
    commits: BTreeMap<(String, String, String), Result<String, String>>,
}

impl Remote {
    fn latest(&mut self, owner: &str, repo: &str) -> Result<Resolved, String> {
        if !self.enabled {
            return Err("not looked up (--no-network)".into());
        }
        let key = (owner.to_string(), repo.to_string());
        if let Some(r) = self.latest.get(&key) {
            return r.clone();
        }
        let r = Self::resolve_latest(owner, repo);
        self.latest.insert(key, r.clone());
        r
    }

    /// The commit SHA `reference` (a tag or a branch) names right now: pinning to it changes no
    /// behaviour, which pinning to the latest release may.
    fn commit_of(&mut self, owner: &str, repo: &str, reference: &str) -> Result<String, String> {
        if !self.enabled {
            return Err("not looked up (--no-network)".into());
        }
        let key = (owner.to_string(), repo.to_string(), reference.to_string());
        if let Some(r) = self.commits.get(&key) {
            return r.clone();
        }
        let r = gh(&[
            "api",
            &format!("repos/{owner}/{repo}/commits/{reference}"),
            "--jq",
            ".sha",
        ])
        .map(|s| s.trim().to_string());
        self.commits.insert(key, r.clone());
        r
    }

    fn resolve_latest(owner: &str, repo: &str) -> Result<Resolved, String> {
        let rel = gh(&[
            "api",
            &format!("repos/{owner}/{repo}/releases/latest"),
            "--jq",
            ".tag_name",
        ]);
        let (tag, how) = match rel {
            Ok(t) if !t.trim().is_empty() => (t.trim().to_string(), "release"),
            _ => {
                let names = gh(&[
                    "api",
                    &format!("repos/{owner}/{repo}/tags?per_page=100"),
                    "--jq",
                    ".[].name",
                ])?;
                let names: Vec<String> = names.lines().map(str::to_string).collect();
                let t = pick_latest_tag(&names).ok_or_else(|| {
                    format!("no release and no version tag among {} tags", names.len())
                })?;
                (t, "highest tag")
            }
        };
        let obj = gh(&[
            "api",
            &format!("repos/{owner}/{repo}/git/ref/tags/{tag}"),
            "--jq",
            ".object.type + \" \" + .object.sha",
        ])?;
        let (ty, sha) = obj
            .trim()
            .split_once(' ')
            .ok_or_else(|| format!("unexpected ref answer {obj:?}"))?;
        let sha = if ty == "tag" {
            // An annotated tag points at a tag object; the commit is one hop further.
            gh(&[
                "api",
                &format!("repos/{owner}/{repo}/git/tags/{sha}"),
                "--jq",
                ".object.sha",
            ])?
            .trim()
            .to_string()
        } else {
            sha.to_string()
        };
        Ok(Resolved { tag, sha, how })
    }

    /// The `runs.using` of `owner/repo/path` at `sha`.
    fn runtime(
        &mut self,
        owner: &str,
        repo: &str,
        path: &str,
        sha: &str,
    ) -> Result<String, String> {
        let key = (owner.into(), repo.into(), path.into(), sha.into());
        if let Some(r) = self.runtime.get(&key) {
            return r.clone();
        }
        let dir = if path.is_empty() {
            String::new()
        } else {
            format!("{path}/")
        };
        let mut last = String::new();
        let mut found = Err(String::new());
        for file in ["action.yml", "action.yaml"] {
            match gh(&[
                "api",
                "-H",
                "Accept: application/vnd.github.raw",
                &format!("repos/{owner}/{repo}/contents/{dir}{file}?ref={sha}"),
            ]) {
                Ok(text) => {
                    found =
                        runs_using(&text).ok_or_else(|| format!("{dir}{file} has no runs.using"));
                    break;
                }
                Err(e) => last = e,
            }
        }
        if let Err(e) = &found {
            if e.is_empty() {
                found = Err(format!("no action.yml or action.yaml: {last}"));
            }
        }
        self.runtime.insert(key, found.clone());
        found
    }
}

#[derive(Default)]
struct Totals {
    repos: usize,
    files: usize,
    uses: usize,
    pinned: usize,
    mutable: usize,
    mutable_tag: usize,
    mutable_branch: usize,
    mutable_other: usize,
    patched: usize,
    unpatchable: usize,
    repos_with_mutable: usize,
    actions: std::collections::BTreeSet<String>,
    local: usize,
    docker: usize,
    unresolved: usize,
    node_flag: usize,
    info: BTreeMap<String, ActionInfo>,
}

/// What is known about one distinct action (`owner/repo[/path]`) across every repo planned.
#[derive(Default)]
struct ActionInfo {
    uses: usize,
    branch_uses: usize,
    kept_branch: usize,
    repos: std::collections::BTreeSet<String>,
    /// `ref` -> runtime at the commit that ref names now.
    now: BTreeMap<String, String>,
    latest: Option<Resolved>,
    latest_note: String,
    latest_runtime: String,
}

/// `node24` -> `Some(24)`; anything else (`composite`, `docker`, `?`) -> `None`.
fn node_version(rt: &str) -> Option<u32> {
    rt.strip_prefix("node")?.parse().ok()
}

fn record(info: &mut BTreeMap<String, ActionInfo>, remote: &mut Remote, u: &Use, repo: &str) {
    let Some((o, r, path)) = u.parts() else {
        return;
    };
    if u.kind == Kind::Unversioned {
        return;
    }
    let e = info.entry(u.target.clone()).or_default();
    e.uses += 1;
    e.repos.insert(repo.to_string());
    if u.kind == Kind::MutableBranch {
        e.branch_uses += 1;
    }
    if !remote.enabled {
        return;
    }
    let reusable = u.is_reusable_workflow();
    if !e.now.contains_key(&u.reference) {
        let rt = if reusable {
            "n/a".to_string()
        } else {
            match remote.commit_of(o, r, &u.reference) {
                Ok(sha) => remote
                    .runtime(o, r, path, &sha)
                    .unwrap_or_else(|_| "?".into()),
                Err(_) => "?".into(),
            }
        };
        e.now.insert(u.reference.clone(), rt);
    }
    if e.latest.is_none() && e.latest_note.is_empty() {
        match remote.latest(o, r) {
            Ok(l) => {
                e.latest_runtime = if reusable {
                    "n/a".into()
                } else {
                    remote
                        .runtime(o, r, path, &l.sha)
                        .unwrap_or_else(|_| "?".into())
                };
                e.latest = Some(l);
            }
            Err(err) => e.latest_note = format!("no release: {err}"),
        }
    }
}

fn verdict(i: &ActionInfo) -> &'static str {
    if i.latest.is_none() {
        return "NO RELEASE";
    }
    match node_version(&i.latest_runtime) {
        Some(n) if n <= 20 => "NO-NODE24",
        Some(_) => "node24+",
        None if i.latest_runtime == "n/a" => "reusable workflow",
        None if i.latest_runtime == "?" => "runtime unread",
        None => "no node runtime",
    }
}

/// `DIR/ACTIONS.md`: every distinct mutable action, those with no node24 release first.
fn write_actions(
    dir: &Path,
    latest_mode: bool,
    info: &BTreeMap<String, ActionInfo>,
) -> Result<(), String> {
    let row = |name: &str, i: &ActionInfo| {
        let now: Vec<String> = i.now.iter().map(|(r, rt)| format!("{r}={rt}")).collect();
        let (tag, rt) = i
            .latest
            .as_ref()
            .map_or(("-".to_string(), i.latest_note.clone()), |l| {
                (l.tag.clone(), i.latest_runtime.clone())
            });
        let cur_majors: std::collections::BTreeSet<u64> =
            i.now.keys().filter_map(|r| major_of(r)).collect();
        let jump = match (major_of(&tag), cur_majors.iter().next_back()) {
            (Some(l), Some(c)) if l != *c || cur_majors.len() > 1 => {
                format!(
                    "MAJOR {}->{l}",
                    cur_majors
                        .iter()
                        .map(u64::to_string)
                        .collect::<Vec<_>>()
                        .join("/")
                )
            }
            (Some(l), Some(_)) => format!("same major {l}"),
            _ => "branch ref".into(),
        };
        format!(
            "| `{}` | {} | {} | {} | {} | {} | {} |",
            cell(name),
            i.uses,
            i.repos.len(),
            cell(&now.join(", ")),
            cell(&tag),
            cell(&rt),
            jump
        )
    };
    let head = "| action | uses | repos | now (ref=runtime) | latest tag | runtime at latest | major |\n|---|---|---|---|---|---|---|";
    let mut out = format!(
        "# Actions with a mutable ref ({} mode)\n\nRuntime = `runs.using` of the action.yml at the commit named. \
         `NO-NODE24` = the latest release itself still declares node20 or older (no upstream fix); \
         `composite`/`docker` run no Node of their own.\n\n",
        if latest_mode { "target latest" } else { "target current" }
    );
    let no24: Vec<_> = info
        .iter()
        .filter(|(_, i)| verdict(i) == "NO-NODE24")
        .collect();
    let norel: Vec<_> = info
        .iter()
        .filter(|(_, i)| verdict(i) == "NO RELEASE")
        .collect();
    out.push_str(&format!(
        "## No node24 release yet ({})\n\n{head}\n",
        no24.len()
    ));
    for (n, i) in &no24 {
        out.push_str(&row(n, i));
        out.push('\n');
    }
    out.push_str(&format!(
        "\n## No release at all ({})\n\n{head}\n",
        norel.len()
    ));
    for (n, i) in &norel {
        out.push_str(&row(n, i));
        out.push('\n');
    }
    let rest: Vec<_> = info
        .iter()
        .filter(|(_, i)| !matches!(verdict(i), "NO-NODE24" | "NO RELEASE"))
        .collect();
    out.push_str(&format!(
        "\n## Everything else ({})\n\n{head}\n",
        rest.len()
    ));
    for (n, i) in &rest {
        out.push_str(&row(n, i));
        out.push('\n');
    }
    let path = dir.join("ACTIONS.md");
    std::fs::write(&path, out).map_err(|e| format!("{}: {e}", path.display()))
}

fn cell(s: &str) -> String {
    s.replace('|', "\\|")
}

fn plan_repo(
    repo: &Path,
    remote: &mut Remote,
    all: bool,
    emit: Option<&Path>,
    latest_mode: bool,
    totals: &mut Totals,
) -> Result<(), String> {
    let name = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Read the index, not the disk, and say which ref the text came from.
    let listing = git(repo, &["ls-files", "-z"])?;
    let files: Vec<&str> = listing
        .split('\0')
        .filter(|f| !f.is_empty() && interesting(f))
        .collect();
    let rev = if git(repo, &["rev-parse", "--verify", "--quiet", "origin/main"]).is_ok() {
        "origin/main"
    } else {
        "HEAD"
    };
    totals.repos += 1;
    totals.files += files.len();
    let mut rows: Vec<(Use, [String; 3])> = Vec::new();
    let mut missing = Vec::new();
    let mut repo_mutable = 0usize;
    let mut patch = String::new();
    for f in &files {
        let text = match git(repo, &["show", &format!("{rev}:{f}")]) {
            Ok(t) => t,
            Err(_) => {
                // In the index but not in the ref: a file only this checkout has. Said, not hidden.
                missing.push(*f);
                continue;
            }
        };
        let lines: Vec<&str> = text.split('\n').collect();
        let mut edits = BTreeMap::new();
        for u in scan(f, &text) {
            totals.uses += 1;
            match u.kind {
                Kind::Pinned => totals.pinned += 1,
                Kind::Local => totals.local += 1,
                Kind::Docker => totals.docker += 1,
                _ => {
                    totals.mutable += 1;
                    repo_mutable += 1;
                    match u.kind {
                        Kind::MutableTag => totals.mutable_tag += 1,
                        Kind::MutableBranch => totals.mutable_branch += 1,
                        _ => totals.mutable_other += 1,
                    }
                    if let Some((o, r, _)) = u.parts() {
                        totals.actions.insert(format!("{o}/{r}"));
                    }
                    record(&mut totals.info, remote, &u, &name);
                    if emit.is_some() {
                        // current: the SHA the ref names right now (no behaviour change). latest:
                        // the latest release's SHA for a tag ref; a branch ref keeps its current SHA.
                        let want_latest = latest_mode && u.kind == Kind::MutableTag;
                        if latest_mode && !want_latest {
                            if let Some(i) = totals.info.get_mut(&u.target) {
                                i.kept_branch += 1;
                            }
                        }
                        let pinned = u
                            .parts()
                            .filter(|_| u.kind != Kind::Unversioned)
                            .and_then(|(o, r, _)| {
                                if want_latest {
                                    remote.latest(o, r).ok().map(|l| (l.sha, l.tag))
                                } else {
                                    remote
                                        .commit_of(o, r, &u.reference)
                                        .ok()
                                        .map(|s| (s, u.reference.clone()))
                                }
                            })
                            .and_then(|(sha, label)| {
                                pin_line_as(lines[u.line - 1], &u, &sha, &label)
                            });
                        match pinned {
                            Some(new) => {
                                edits.insert(u.line, new);
                                totals.patched += 1;
                            }
                            None => totals.unpatchable += 1,
                        }
                    }
                }
            }
            let show = u.is_mutable() || all;
            if !show {
                continue;
            }
            let note = if u.is_mutable() {
                describe(&u, remote, totals)
            } else {
                ["-".into(), "-".into(), "-".into()]
            };
            rows.push((u, note));
        }
        patch.push_str(&unified_diff(f, &text, &edits));
    }
    if let (Some(dir), false) = (emit, patch.is_empty()) {
        let out = dir.join(format!("{name}.patch"));
        std::fs::write(&out, &patch).map_err(|e| format!("{}: {e}", out.display()))?;
        println!("Patch: `{}`.", out.display());
        println!();
    }
    if repo_mutable > 0 {
        totals.repos_with_mutable += 1;
    }
    println!("## {name}");
    println!();
    println!(
        "{} workflow/action file(s) from `{rev}`; {} reference(s) listed.{}",
        files.len() - missing.len(),
        rows.len(),
        if missing.is_empty() {
            String::new()
        } else {
            format!(
                " Not at `{rev}` (checkout-only, not read): {}.",
                missing.join(", ")
            )
        }
    );
    println!();
    if rows.is_empty() {
        println!("Nothing to pin.");
        println!();
        return Ok(());
    }
    println!(
        "| file:line | uses | now | SHA of the current ref | latest release (tag, SHA) | node now -> latest |"
    );
    println!("|---|---|---|---|---|---|");
    for (u, [cur, latest, node]) in rows {
        let now = match u.kind {
            Kind::Pinned => "sha",
            Kind::MutableTag => "tag",
            Kind::MutableBranch => "branch",
            Kind::Unversioned => "none",
            Kind::Local => "local",
            Kind::Docker => "docker",
        };
        println!(
            "| {}:{} | `{}` | {now} | {} | {} | {} |",
            cell(&u.file),
            u.line,
            cell(&u.raw),
            cell(&cur),
            cell(&latest),
            cell(&node),
        );
    }
    println!();
    Ok(())
}

/// The three cells for a mutable reference: the SHA its current ref names, the latest release
/// (tag, how it was chosen, SHA), and the Node runtime now and at the latest release. Counts
/// failed lookups into `totals.unresolved` (not when `--no-network`) and node20-or-older of what
/// runs today into `totals.node_flag`. A failed lookup is the text of its own cell.
fn describe(u: &Use, remote: &mut Remote, totals: &mut Totals) -> [String; 3] {
    let Some((owner, repo, path)) = u.parts() else {
        totals.unresolved += 1;
        return ["-".into(), "-".into(), "not an owner/repo reference".into()];
    };
    let enabled = remote.enabled;
    let unresolved = |e: String, totals: &mut Totals| {
        if enabled {
            totals.unresolved += 1;
        }
        format!("UNRESOLVED: {e}")
    };
    let cur = match remote.commit_of(owner, repo, &u.reference) {
        Ok(s) => s,
        Err(e) => return [unresolved(e, totals), "-".into(), "-".into()],
    };
    let latest = match remote.latest(owner, repo) {
        Ok(r) => r,
        Err(e) => return [cur, unresolved(e, totals), "-".into()],
    };
    let node = if u.is_reusable_workflow() {
        "n/a (reusable workflow)".to_string()
    } else {
        let now = remote.runtime(owner, repo, path, &cur);
        let then = remote.runtime(owner, repo, path, &latest.sha);
        match (&now, &then) {
            (Err(e), Err(_)) => unresolved(e.clone(), totals),
            _ => {
                let show = |r: &Result<String, String>| r.clone().unwrap_or_else(|_| "?".into());
                let flag = matches!(&now, Ok(v) if runtime_needs_attention(v));
                let text = format!("{} -> {}", show(&now), show(&then));
                if flag {
                    totals.node_flag += 1;
                    format!("**{text} - FLAG**")
                } else {
                    text
                }
            }
        }
    };
    [
        cur,
        format!("{} ({}) {}", latest.tag, latest.how, latest.sha),
        node,
    ]
}

/// Creates `dir` unless it lies inside one of `repos` (judged by its nearest existing ancestor,
/// canonicalised, so a not-yet-created subfolder of a repo is refused too).
fn prepare_patch_dir(dir: &Path, repos: &[PathBuf]) -> Result<(), String> {
    let mut probe = std::path::absolute(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    while !probe.exists() {
        if !probe.pop() {
            return Err(format!("{}: no existing ancestor", dir.display()));
        }
    }
    let probe = probe
        .canonicalize()
        .map_err(|e| format!("{}: {e}", probe.display()))?;
    for r in repos {
        if let Ok(r) = r.canonicalize() {
            if probe.starts_with(&r) {
                return Err(format!(
                    "{} is inside the repository {}: patches are never written into a repo",
                    dir.display(),
                    r.display()
                ));
            }
        }
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("pin-plan: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    let root = args
        .root
        .clone()
        .unwrap_or_else(|| PathBuf::from("C:/Projects"));
    let (repos, skipped) = match repos_under(&root, &args.repos) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("pin-plan: {e}");
            std::process::exit(1);
        }
    };
    if let Some(dir) = &args.emit {
        if args.no_network {
            eprintln!("pin-plan: --emit-patch needs the network (drop --no-network)");
            std::process::exit(2);
        }
        if let Err(e) = prepare_patch_dir(dir, &repos) {
            eprintln!("pin-plan: {e}");
            std::process::exit(2);
        }
    }
    println!("# SUP-008 pin plan (read-only)");
    println!();
    println!(
        "Root `{}`: {} repo(s) planned; skipped {}: {}. Files from `git ls-files`, text from \
         `origin/main` (else `HEAD`), release data from `gh api` ({}).",
        root.display(),
        repos.len(),
        skipped.len(),
        if skipped.is_empty() {
            "none".into()
        } else {
            skipped.join(", ")
        },
        if args.no_network {
            "not called"
        } else {
            "live"
        }
    );
    println!();
    let mut remote = Remote {
        enabled: !args.no_network,
        latest: BTreeMap::new(),
        runtime: BTreeMap::new(),
        commits: BTreeMap::new(),
    };
    let mut totals = Totals::default();
    let mut failed = Vec::new();
    for repo in &repos {
        if let Err(e) = plan_repo(
            repo,
            &mut remote,
            args.all,
            args.emit.as_deref(),
            args.latest,
            &mut totals,
        ) {
            let name = repo
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            println!("## {name}\n\nCOULD NOT READ: {e}\n");
            failed.push(name);
        }
    }
    if let Some(dir) = &args.emit {
        if let Err(e) = write_actions(dir, args.latest, &totals.info) {
            eprintln!("pin-plan: {e}");
            failed.push("ACTIONS.md".into());
        }
    }
    println!("## Totals\n");
    println!(
        "{} repo(s) read ({} failed), {} workflow/action file(s), {} `uses:` reference(s): {} already \
         pinned, **{} mutable** ({} tag, {} branch, {} no ref) in {} repo(s) across {} distinct \
         action repo(s), {} local, {} docker. {} mutable reference(s) unresolved, {} on node20 or \
         older.{}",
        totals.repos,
        failed.len(),
        totals.files,
        totals.uses,
        totals.pinned,
        totals.mutable,
        totals.mutable_tag,
        totals.mutable_branch,
        totals.mutable_other,
        totals.repos_with_mutable,
        totals.actions.len(),
        totals.local,
        totals.docker,
        totals.unresolved,
        totals.node_flag,
        if args.emit.is_some() {
            format!(
                " Patches: {} reference(s) pinned, {} could not be.",
                totals.patched, totals.unpatchable
            )
        } else {
            String::new()
        }
    );
    std::process::exit(if failed.is_empty() { 0 } else { 1 });
}
