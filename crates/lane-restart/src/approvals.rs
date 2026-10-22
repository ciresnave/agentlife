// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where active handlers come from — RESTART-TOOL-DESIGN.md §12.3.
//!
//! ⚠️ ONLY the repo + path the USER configured, ONLY that repo's default
//! branch (CIRESNAVE-EXPECTATIONS.md §5.1c, 2026-09-27). OverMind is public:
//! approvals built into it would make one user's decisions every user's
//! default. Each user names their own approvals repo in
//! `~/.overmind/lane-restart.json`; no config means no handlers, and every
//! failure along the way (no `gh`, no network, a 404, a bad file) means
//! fewer handlers, never a guessed one.
//!
//! ⚠️ The commit is resolved from the default branch HERE, never taken from
//! anywhere else: GitHub serves a fork's commits through the parent repo's
//! API too, so a caller-supplied ref could name an unmerged fork PR's
//! content. Merged content is what the owner approved; a PR is not.

use crate::handlers::{self, HandlerSpec};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// `~/.overmind/lane-restart.json`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub approvals: ApprovalsSource,
    /// The lane role told about a startup dialog no approval matched
    /// (§12.6). Defaults to `pm`.
    #[serde(default = "default_notify_role")]
    pub notify_role: String,
    /// How an [ASK] reaches `notify_role` (M8: claude-peers is going away). `broker` (default) is
    /// the claude-peers broker; `synapse` queues the message in the notify outbox, which the
    /// launcher service relays as role `agentlife-launcher`.
    #[serde(default)]
    pub notify_transport: crate::notify::Transport,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalsSource {
    /// `owner/name`.
    pub repo: String,
    /// The folder inside `repo` holding one `<id>.json` per approval, e.g.
    /// `.overmind/lane-restart/approvals`.
    pub path: String,
}

fn default_notify_role() -> String {
    "pm".to_string()
}

/// `notify_transport` from the config under `home`; `broker` when there is no home, no file or
/// the file is invalid (the claude-peers path stays the default until the cutover flips it).
pub fn configured_transport(home: Option<&Path>) -> crate::notify::Transport {
    home.and_then(|h| load_config(&config_path(h)).ok())
        .map_or(crate::notify::Transport::Broker, |c| c.notify_transport)
}

pub fn config_path(home: &Path) -> PathBuf {
    home.join(".overmind").join("lane-restart.json")
}

/// `owner/name`, each part GitHub-legal (letters, digits, `-`, and for the
/// name also `.` and `_`), never `.` or `..`.
pub fn valid_repo(repo: &str) -> bool {
    let Some((owner, name)) = repo.split_once('/') else {
        return false;
    };
    let owner_ok = !owner.is_empty()
        && owner.len() <= 39
        && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    let name_ok = !name.is_empty()
        && name.len() <= 100
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_');
    owner_ok && name_ok
}

/// A relative folder path: `/`-separated segments of letters, digits, `.`,
/// `_` and `-`, none empty, `.` or `..`. Nothing that needs URL-escaping.
pub fn valid_repo_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && seg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
        })
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// No config file: the user hasn't chosen an approvals repo. Not an
    /// error in the tool - just nothing active.
    NotConfigured(PathBuf),
    Invalid(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::NotConfigured(p) => {
                write!(
                    f,
                    "no approvals repo configured ({} does not exist)",
                    p.display()
                )
            }
            ConfigError::Invalid(e) => write!(f, "approvals config is invalid: {e}"),
        }
    }
}

pub fn load_config(path: &Path) -> Result<Config, ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ConfigError::NotConfigured(path.to_path_buf()))
        }
        Err(e) => return Err(ConfigError::Invalid(format!("{}: {e}", path.display()))),
    };
    let config: Config = serde_json::from_str(&text)
        .map_err(|e| ConfigError::Invalid(format!("{}: {e}", path.display())))?;
    if !valid_repo(&config.approvals.repo) {
        return Err(ConfigError::Invalid(format!(
            "approvals.repo {:?} is not owner/name",
            config.approvals.repo
        )));
    }
    if !valid_repo_path(&config.approvals.path) {
        return Err(ConfigError::Invalid(format!(
            "approvals.path {:?} is not a plain relative folder path",
            config.approvals.path
        )));
    }
    if !crate::handlers::valid_id(&config.notify_role) {
        return Err(ConfigError::Invalid(format!(
            "notify_role {:?} is not a plain role name",
            config.notify_role
        )));
    }
    Ok(config)
}

/// One entry of a GitHub contents-API directory listing.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize)]
pub struct DirEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub size: u64,
}

/// A folder as ONE round trip returned it: the default branch's tip, the listing, and every file's
/// bytes (`None` for a file too large, truncated or binary to have been delivered).
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub commit: String,
    pub entries: Vec<(DirEntry, Option<Vec<u8>>)>,
}

/// Read-only access to one GitHub repo. A trait so the loading rules can be
/// tested without a network; [`GhReader`] is the real one.
pub trait RepoReader {
    /// The commit SHA at the tip of `repo`'s DEFAULT branch.
    fn default_branch_commit(&self, repo: &str) -> Result<String, String>;
    fn list_dir(&self, repo: &str, path: &str, commit: &str) -> Result<Vec<DirEntry>, String>;
    fn read_file(&self, repo: &str, path: &str, commit: &str) -> Result<Vec<u8>, String>;
    /// The tip, the listing and every file in ONE call, if this reader can: `None` means it cannot
    /// (use [`default_branch_commit`](Self::default_branch_commit), [`list_dir`](Self::list_dir) and
    /// [`read_file`](Self::read_file)). 2026-10-22: one `gh` read per file let five approvals starve a
    /// 20 s budget; a snapshot makes the cost of a load independent of how many approvals exist.
    fn snapshot(&self, _repo: &str, _path: &str) -> Option<Result<Snapshot, String>> {
        None
    }
    /// Set when the last [`default_branch_commit`](Self::default_branch_commit) answer was NOT read
    /// from GitHub but taken from a remembered, fully loaded commit (see [`CachingReader`]).
    fn stale_note(&self) -> Option<String> {
        None
    }
    /// Told by [`load`] that `commit` loaded completely (no error), so a reader may remember it.
    fn record_good(&self, _repo: &str, _commit: &str) {}
}

/// At most this many approval files are read; more is refused outright.
pub const MAX_FILES: usize = 64;
/// Any file larger than this is refused unread.
pub const MAX_FILE_BYTES: u64 = 64 * 1024;

/// What loading produced: the active handlers, what was refused and why,
/// and exactly where it all came from.
#[derive(Debug, Default)]
pub struct Loaded {
    /// `owner/name:path@commit`, or `None` if nothing was fetched.
    pub source: Option<String>,
    /// (handler, sha256 of its exact file bytes)
    pub handlers: Vec<(HandlerSpec, String)>,
    /// (file name, reason)
    pub refused: Vec<(String, String)>,
    /// Why NOTHING loaded, when that's the case.
    pub error: Option<String>,
    /// Set when GitHub could not be reached and the last fully loaded approvals commit was used
    /// instead: how old that check is and why. `None` means the branch tip was read live.
    pub stale: Option<String>,
}

impl Loaded {
    pub fn specs(&self) -> Vec<HandlerSpec> {
        self.handlers.iter().map(|(h, _)| h.clone()).collect()
    }

    /// One line for logs and notifications.
    pub fn summary(&self) -> String {
        match (&self.source, &self.error) {
            (_, Some(e)) => format!("0 active - {e}"),
            (Some(src), None) => format!(
                "{} active, {} refused, from {src} [{}]",
                self.handlers.len(),
                self.refused.len(),
                match &self.stale {
                    Some(note) => format!("STALE: {note}"),
                    None => "tip read live".to_string(),
                }
            ),
            (None, None) => "0 active".to_string(),
        }
    }
}

/// Every `*.json` file directly inside `source.path` on the default branch,
/// each parsed and validated on its own (`handlers::parse_and_validate`),
/// plus the rule that a file's name is its handler's `id` - so the file a
/// reviewer sees in the PR is the handler the log names.
pub fn load(source: &ApprovalsSource, reader: &dyn RepoReader) -> Loaded {
    let mut loaded = Loaded::default();
    if let Some(snapshot) = reader.snapshot(&source.repo, &source.path) {
        let snap = match snapshot {
            Ok(s) => s,
            Err(e) => {
                loaded.error = Some(format!(
                    "could not resolve {}'s default branch: {e}",
                    source.repo
                ));
                return loaded;
            }
        };
        if !is_sha(&snap.commit) {
            loaded.error = Some(format!(
                "default branch resolved to {:?}, not a commit SHA",
                snap.commit
            ));
            return loaded;
        }
        loaded.source = Some(format!(
            "{}:{}@{}",
            source.repo,
            source.path,
            &snap.commit[..12]
        ));
        loaded.stale = reader.stale_note();
        let mut bodies: std::collections::HashMap<String, Option<Vec<u8>>> = Default::default();
        let mut entries = Vec::new();
        for (entry, body) in snap.entries {
            bodies.insert(entry.name.clone(), body);
            entries.push(entry);
        }
        load_entries(&entries, &mut loaded, &mut |name| {
            bodies
                .remove(name)
                .flatten()
                .ok_or_else(|| "its content was not delivered with the folder".to_string())
        });
        if loaded.error.is_none() {
            reader.record_good(&source.repo, &snap.commit);
        }
        return loaded;
    }
    let commit = match reader.default_branch_commit(&source.repo) {
        Ok(c) if is_sha(&c) => c,
        Ok(c) => {
            loaded.error = Some(format!(
                "default branch resolved to {c:?}, not a commit SHA"
            ));
            return loaded;
        }
        Err(e) => {
            loaded.error = Some(format!(
                "could not resolve {}'s default branch: {e}",
                source.repo
            ));
            return loaded;
        }
    };
    loaded.source = Some(format!("{}:{}@{}", source.repo, source.path, &commit[..12]));
    loaded.stale = reader.stale_note();
    let entries = match reader.list_dir(&source.repo, &source.path, &commit) {
        Ok(e) => e,
        Err(e) => {
            loaded.error = Some(format!("could not list {}: {e}", source.path));
            return loaded;
        }
    };
    load_entries(&entries, &mut loaded, &mut |name| {
        reader.read_file(&source.repo, &format!("{}/{name}", source.path), &commit)
    });
    if loaded.error.is_none() {
        reader.record_good(&source.repo, &commit);
    }
    loaded
}

fn is_sha(c: &str) -> bool {
    c.len() == 40 && c.chars().all(|ch| ch.is_ascii_hexdigit())
}

/// Parses and validates every `*.json` entry, reading each body through `fetch`.
fn load_entries(
    entries: &[DirEntry],
    loaded: &mut Loaded,
    fetch: &mut dyn FnMut(&str) -> Result<Vec<u8>, String>,
) {
    let json_files: Vec<&DirEntry> = entries
        .iter()
        .filter(|e| e.kind == "file" && e.name.ends_with(".json"))
        .collect();
    if json_files.len() > MAX_FILES {
        loaded.error = Some(format!(
            "{} approval files exceeds the limit of {MAX_FILES}",
            json_files.len()
        ));
        return;
    }
    for entry in json_files {
        let name = entry.name.clone();
        if entry.size > MAX_FILE_BYTES {
            loaded.refused.push((
                name,
                format!("{} bytes exceeds {MAX_FILE_BYTES}", entry.size),
            ));
            continue;
        }
        let bytes = match fetch(&name) {
            Ok(b) => b,
            Err(e) => {
                loaded.refused.push((name, format!("could not read: {e}")));
                continue;
            }
        };
        let Ok(text) = String::from_utf8(bytes) else {
            loaded.refused.push((name, "not UTF-8".into()));
            continue;
        };
        match handlers::parse_and_validate(&text) {
            Ok(h) if format!("{}.json", h.id) != name => loaded
                .refused
                .push((name, format!("file name must be \"{}.json\", its id", h.id))),
            Ok(h) => {
                let hash = handlers::handler_content_hash(&text);
                loaded.handlers.push((h, hash));
            }
            Err(reason) => loaded.refused.push((name, reason)),
        }
    }
}

/// Loads the user's config from `home` and then the approvals it names.
/// Returns the config too (when there is one) so the caller knows who to
/// notify.
pub fn load_configured(home: &Path, reader: &dyn RepoReader) -> (Option<Config>, Loaded) {
    match load_config(&config_path(home)) {
        Ok(config) => {
            let loaded = load(&config.approvals, reader);
            (Some(config), loaded)
        }
        Err(e) => (
            None,
            Loaded {
                error: Some(e.to_string()),
                ..Loaded::default()
            },
        ),
    }
}

/// The real [`RepoReader`]: `gh api`, which carries the user's own GitHub
/// auth (so a private approvals repo works too). Every call has a hard
/// timeout; `gh` is never allowed to prompt.
pub struct GhReader {
    /// The limit of one `gh` call.
    pub timeout: Duration,
    /// The limit of ALL calls of this reader together, counted from its construction. A host loads
    /// approvals before it starts claude, so this is how long a slow GitHub can delay a lane: the
    /// per-call limit alone allowed `15 s x calls` (2026-10-17/18: 10 restarts hit the 20 s verdict
    /// with the load at 19.5-44.5 s; it takes 4-8 s when GitHub answers).
    pub budget: Duration,
    started: Instant,
}

/// The default budget of a whole approvals load.
///
/// 20 s (was 8 s): a slow link made the two sequential tip calls time out (2026-10-21/22), and the
/// appear verdict that the 8 s protected is 120 s now. The tip is ONE call, and a failed one falls
/// back to the last fully loaded commit (see [`CachingReader`]), so this only bounds the worst case.
pub const LOAD_BUDGET: Duration = Duration::from_secs(20);

impl Default for GhReader {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(15),
            budget: LOAD_BUDGET,
            started: Instant::now(),
        }
    }
}

impl GhReader {
    fn gh(&self, args: &[&str]) -> Result<Vec<u8>, String> {
        let left = self.budget.saturating_sub(self.started.elapsed());
        if left.is_zero() {
            return Err(format!(
                "the {}s approvals budget is used up",
                self.budget.as_secs()
            ));
        }
        run_with_timeout("gh", args, self.timeout.min(left))
    }
}

/// How old a remembered approvals commit may be when it stands in for an unreachable GitHub.
/// CireSnave-approved by the PM 2026-10-22: an approval removed from the repo stays honoured for at
/// most this long, and only while GitHub cannot be reached from this machine.
pub const MAX_STALE: Duration = Duration::from_secs(24 * 60 * 60);

/// Does this `gh` failure say the network (or GitHub) is down, as opposed to GitHub answering "no"?
/// Only these may fall back to a remembered commit; anything unrecognised (a 404, 401, 403, a rate
/// limit, a changed message) keeps the load failing closed.
pub fn is_transient(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    [
        "timed out",
        "i/o timeout",
        "error connecting",
        "dial tcp",
        "no such host",
        "could not resolve host",
        "network is unreachable",
        "connection reset",
        "tls handshake",
        "http 500",
        "http 502",
        "http 503",
        "http 504",
    ]
    .iter()
    .any(|t| e.contains(t))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The remembered commit: written only when a load of it completed with no error.
#[derive(Debug, Deserialize, serde::Serialize)]
struct GoodPointer {
    repo: String,
    commit: String,
    verified_at: u64,
}

/// A [`RepoReader`] that remembers directory listings and files by COMMIT SHA, which names
/// immutable content: a hit needs no network, and a changed approvals folder is a new SHA, so it
/// misses. The branch-tip lookup always goes out first; if it fails with a TRANSIENT error and a
/// commit whose load completed within [`MAX_STALE`] is remembered, that commit stands in (its
/// listing and files come from this cache, and `load` still validates every byte). Entries are
/// named by a hash of `repo|path|commit`, never by the path itself. A cache that cannot be read or
/// written is a miss.
pub struct CachingReader<R> {
    inner: R,
    dir: PathBuf,
    now: fn() -> u64,
    /// Set while the last tip answer was the remembered one; read by `stale_note`.
    stale: std::cell::RefCell<Option<String>>,
}

impl<R: RepoReader> CachingReader<R> {
    pub fn new(inner: R, dir: PathBuf) -> Self {
        Self::with_clock(inner, dir, unix_now)
    }

    /// [`new`](Self::new) with an injected clock (seconds since the Unix epoch), for tests.
    pub fn with_clock(inner: R, dir: PathBuf, now: fn() -> u64) -> Self {
        Self {
            inner,
            dir,
            now,
            stale: std::cell::RefCell::new(None),
        }
    }

    fn pointer_path(&self, repo: &str) -> PathBuf {
        use sha2::{Digest, Sha256};
        let h = Sha256::digest(format!("last-good|{repo}").as_bytes());
        let hex: String = h.iter().map(|b| format!("{b:02x}")).collect();
        self.dir.join(format!("{hex}.good"))
    }

    /// The remembered commit for `repo`, if it is well-formed and names this repo.
    fn pointer(&self, repo: &str) -> Option<GoodPointer> {
        let p: GoodPointer =
            serde_json::from_slice(&std::fs::read(self.pointer_path(repo)).ok()?).ok()?;
        let hex40 = p.commit.len() == 40 && p.commit.chars().all(|c| c.is_ascii_hexdigit());
        (p.repo == repo && hex40).then_some(p)
    }

    fn entry(&self, kind: &str, repo: &str, path: &str, commit: &str) -> Option<PathBuf> {
        use sha2::{Digest, Sha256};
        if commit.len() != 40 || !commit.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let h = Sha256::digest(format!("{kind}|{repo}|{path}|{commit}").as_bytes());
        let hex: String = h.iter().map(|b| format!("{b:02x}")).collect();
        Some(self.dir.join(format!("{hex}.cache")))
    }

    fn get(&self, p: &Option<PathBuf>) -> Option<Vec<u8>> {
        std::fs::read(p.as_ref()?).ok()
    }

    fn put(&self, p: &Option<PathBuf>, bytes: &[u8]) {
        let Some(p) = p else { return };
        let _ = std::fs::create_dir_all(&self.dir);
        let tmp = p.with_extension(format!("tmp{}", std::process::id()));
        if std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, p).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

impl<R: RepoReader> CachingReader<R> {
    /// The tip lookup failed with `error`: a transient failure may fall back to the remembered commit
    /// (marked stale); anything else, or nothing fresh remembered, fails closed with `error` in the message.
    fn fallback_commit(&self, repo: &str, error: String) -> Result<String, String> {
        if !is_transient(&error) {
            return Err(error);
        }
        let Some(good) = self.pointer(repo) else {
            return Err(format!(
                "{error}; no remembered approvals commit to fall back on"
            ));
        };
        let age = Duration::from_secs((self.now)().saturating_sub(good.verified_at));
        if age > MAX_STALE {
            return Err(format!(
                "{error}; the remembered approvals commit is {} old (limit {})",
                human(age),
                human(MAX_STALE)
            ));
        }
        *self.stale.borrow_mut() = Some(format!(
            "GitHub unreachable ({error}); using the approvals commit last fully loaded {} ago",
            human(age)
        ));
        Ok(good.commit)
    }

    /// The snapshot call failed: rebuild it from the remembered commit's cached listing and files, if
    /// the failure is transient and that commit is fresh; otherwise fail closed.
    fn snapshot_from_memory(
        &self,
        repo: &str,
        path: &str,
        error: String,
    ) -> Result<Snapshot, String> {
        let commit = self.fallback_commit(repo, error)?;
        let listing: Vec<DirEntry> = self
            .get(&self.entry("dir", repo, path, &commit))
            .and_then(|b| serde_json::from_slice(&b).ok())
            .ok_or_else(|| "the remembered commit's listing is not cached".to_string())?;
        let entries = listing
            .into_iter()
            .map(|e| {
                let body =
                    self.get(&self.entry("file", repo, &format!("{path}/{}", e.name), &commit));
                (e, body)
            })
            .collect();
        Ok(Snapshot { commit, entries })
    }
}

impl<R: RepoReader> RepoReader for CachingReader<R> {
    fn default_branch_commit(&self, repo: &str) -> Result<String, String> {
        *self.stale.borrow_mut() = None;
        match self.inner.default_branch_commit(repo) {
            Ok(commit) => Ok(commit),
            Err(error) => self.fallback_commit(repo, error),
        }
    }

    fn snapshot(&self, repo: &str, path: &str) -> Option<Result<Snapshot, String>> {
        *self.stale.borrow_mut() = None;
        let result = self.inner.snapshot(repo, path)?;
        Some(match result {
            Ok(snap) => {
                let listing: Vec<DirEntry> = snap.entries.iter().map(|(e, _)| e.clone()).collect();
                if let Ok(bytes) = serde_json::to_vec(&listing) {
                    self.put(&self.entry("dir", repo, path, &snap.commit), &bytes);
                }
                for (entry, body) in &snap.entries {
                    if let Some(body) = body {
                        let p = format!("{path}/{}", entry.name);
                        self.put(&self.entry("file", repo, &p, &snap.commit), body);
                    }
                }
                Ok(snap)
            }
            Err(error) => self.snapshot_from_memory(repo, path, error),
        })
    }

    fn stale_note(&self) -> Option<String> {
        self.stale.borrow().clone()
    }

    fn record_good(&self, repo: &str, commit: &str) {
        // A stand-in commit must not refresh its own "verified" time.
        if self.stale.borrow().is_some() {
            return;
        }
        let pointer = GoodPointer {
            repo: repo.to_owned(),
            commit: commit.to_owned(),
            verified_at: (self.now)(),
        };
        if let Ok(bytes) = serde_json::to_vec(&pointer) {
            self.put(&Some(self.pointer_path(repo)), &bytes);
        }
    }

    fn list_dir(&self, repo: &str, path: &str, commit: &str) -> Result<Vec<DirEntry>, String> {
        let p = self.entry("dir", repo, path, commit);
        if let Some(entries) = self
            .get(&p)
            .and_then(|b| serde_json::from_slice::<Vec<DirEntry>>(&b).ok())
        {
            return Ok(entries);
        }
        let entries = self.inner.list_dir(repo, path, commit)?;
        if let Ok(b) = serde_json::to_vec(&entries) {
            self.put(&p, &b);
        }
        Ok(entries)
    }

    fn read_file(&self, repo: &str, path: &str, commit: &str) -> Result<Vec<u8>, String> {
        let p = self.entry("file", repo, path, commit);
        if let Some(b) = self.get(&p) {
            return Ok(b);
        }
        let b = self.inner.read_file(repo, path, commit)?;
        self.put(&p, &b);
        Ok(b)
    }
}

fn human(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=119 => format!("{s}s"),
        120..=7199 => format!("{}min", s / 60),
        _ => format!("{}h{:02}min", s / 3600, (s % 3600) / 60),
    }
}

/// The reader a host (and `lane-restart approvals`) uses: `gh` under [`LOAD_BUDGET`], cached by
/// commit under `<home>/.overmind/lane-restart-approvals-cache`.
pub fn default_reader(home: &Path) -> CachingReader<GhReader> {
    CachingReader::new(
        GhReader::default(),
        home.join(".overmind").join("lane-restart-approvals-cache"),
    )
}

#[derive(Deserialize)]
struct CommitInfo {
    sha: String,
}

/// The whole approvals folder in one GraphQL call: `HEAD` is the default branch, so `head.oid` is the
/// tip and `dir` is the folder at it, with every blob's text. Both are resolved by the one query.
const SNAPSHOT_QUERY: &str = "query($owner:String!,$name:String!,$expr:String!){repository(owner:$owner,name:$name){head:object(expression:\"HEAD\"){oid} dir:object(expression:$expr){... on Tree{entries{name type object{... on Blob{byteSize isTruncated isBinary text}}}}}}}";

#[derive(Deserialize)]
struct GqlResponse {
    data: Option<GqlData>,
    errors: Option<Vec<GqlError>>,
}
#[derive(Deserialize)]
struct GqlError {
    message: String,
}
#[derive(Deserialize)]
struct GqlData {
    repository: Option<GqlRepo>,
}
#[derive(Deserialize)]
struct GqlRepo {
    head: Option<GqlOid>,
    dir: Option<GqlTree>,
}
#[derive(Deserialize)]
struct GqlOid {
    oid: Option<String>,
}
#[derive(Deserialize)]
struct GqlTree {
    entries: Option<Vec<GqlEntry>>,
}
#[derive(Deserialize)]
struct GqlEntry {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    object: Option<GqlBlob>,
}
#[derive(Deserialize)]
struct GqlBlob {
    #[serde(rename = "byteSize")]
    byte_size: Option<u64>,
    #[serde(rename = "isTruncated")]
    is_truncated: Option<bool>,
    #[serde(rename = "isBinary")]
    is_binary: Option<bool>,
    text: Option<String>,
}

/// Turns the GraphQL answer into a [`Snapshot`]. A file that was not delivered whole (too large,
/// truncated, binary) has no body, and `load` refuses it by name.
pub fn parse_snapshot(body: &[u8], path: &str) -> Result<Snapshot, String> {
    let resp: GqlResponse =
        serde_json::from_slice(body).map_err(|e| format!("unexpected GraphQL response: {e}"))?;
    let repo = resp.data.and_then(|d| d.repository);
    let Some(repo) = repo else {
        let why = resp
            .errors
            .and_then(|e| e.into_iter().next())
            .map_or_else(|| "repository not found".to_string(), |e| e.message);
        return Err(why);
    };
    let commit = repo
        .head
        .and_then(|h| h.oid)
        .ok_or_else(|| "the repository has no default branch tip".to_string())?;
    let tree = repo
        .dir
        .and_then(|d| d.entries)
        .ok_or_else(|| format!("{path} is not a folder"))?;
    let entries = tree
        .into_iter()
        .map(|e| {
            let blob = e.object;
            let size = blob.as_ref().and_then(|b| b.byte_size).unwrap_or(0);
            let whole = blob
                .as_ref()
                .is_some_and(|b| b.is_truncated != Some(true) && b.is_binary != Some(true));
            let bytes = blob
                .and_then(|b| b.text)
                .filter(|_| whole)
                .map(String::into_bytes);
            let kind = if e.kind == "blob" {
                "file".to_string()
            } else {
                e.kind
            };
            (
                DirEntry {
                    name: e.name,
                    kind,
                    size,
                },
                bytes,
            )
        })
        .collect();
    Ok(Snapshot { commit, entries })
}

impl RepoReader for GhReader {
    /// One call for everything (see [`SNAPSHOT_QUERY`]).
    fn snapshot(&self, repo: &str, path: &str) -> Option<Result<Snapshot, String>> {
        let Some((owner, name)) = repo.split_once('/') else {
            return Some(Err(format!("{repo:?} is not owner/name")));
        };
        if !valid_repo(repo) || !valid_repo_path(path) {
            return Some(Err("unusable repository or folder name".into()));
        }
        let body = match self.gh(&[
            "api",
            "graphql",
            "-f",
            &format!("query={SNAPSHOT_QUERY}"),
            "-f",
            &format!("owner={owner}"),
            "-f",
            &format!("name={name}"),
            "-f",
            &format!("expr=HEAD:{path}"),
        ]) {
            Ok(b) => b,
            Err(e) => return Some(Err(e)),
        };
        Some(parse_snapshot(&body, path))
    }

    /// One call: `repos/{repo}/commits/HEAD`. A repository's HEAD IS its default branch, so this is
    /// the default branch's tip (checked equal to `repos/{repo}/branches/<default>` for
    /// `ciresnave/ciresnave` on 2026-10-22), and it needs one network round trip, not two.
    fn default_branch_commit(&self, repo: &str) -> Result<String, String> {
        let commit: CommitInfo =
            serde_json::from_slice(&self.gh(&["api", &format!("repos/{repo}/commits/HEAD")])?)
                .map_err(|e| format!("unexpected commit response: {e}"))?;
        Ok(commit.sha)
    }

    fn list_dir(&self, repo: &str, path: &str, commit: &str) -> Result<Vec<DirEntry>, String> {
        let body = self.gh(&["api", &format!("repos/{repo}/contents/{path}?ref={commit}")])?;
        serde_json::from_slice(&body).map_err(|_| format!("{path} is not a folder"))
    }

    fn read_file(&self, repo: &str, path: &str, commit: &str) -> Result<Vec<u8>, String> {
        self.gh(&[
            "api",
            "-H",
            "Accept: application/vnd.github.raw",
            &format!("repos/{repo}/contents/{path}?ref={commit}"),
        ])
    }
}

/// Runs `program args`, returning stdout on exit 0 and the first line of
/// stderr otherwise; kills it at `timeout`. stdin is closed so nothing can
/// wait on a prompt.
fn run_with_timeout(program: &str, args: &[&str], timeout: Duration) -> Result<Vec<u8>, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new(program)
        .args(args)
        .env("GH_PROMPT_DISABLED", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    let out_thread = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = stdout.read_to_end(&mut v);
        v
    });
    let err_thread = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = stderr.read_to_end(&mut v);
        v
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{program} timed out after {}s", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("{program}: {e}")),
        }
    };
    let out = out_thread.join().unwrap_or_default();
    let err = err_thread.join().unwrap_or_default();
    if status.success() {
        Ok(out)
    } else {
        let msg = String::from_utf8_lossy(&err);
        Err(msg.lines().next().unwrap_or("failed").trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn approval(id: &str) -> String {
        format!(
            r#"{{
                "id": "{id}",
                "match": {{"text_anchors": ["WARNING: Loading development channels"], "fields": {{}}}},
                "action": "1\r",
                "scope": {{"roles": ["*"]}},
                "provenance": {{"approved_by": "CireSnave",
                    "approvals": [{{"said_on": "2026-09-27", "quote": "q", "approving": "a"}}]}}
            }}"#
        )
    }

    /// An in-memory repo: `files` maps a full path to its bytes; the
    /// default branch resolves to `commit`, and reads with any other ref
    /// fail - so a test proves `load` asked for exactly that commit.
    struct FakeRepo {
        commit: Result<String, String>,
        files: HashMap<String, Vec<u8>>,
        dirs: HashMap<String, Vec<DirEntry>>,
    }

    impl FakeRepo {
        fn with_files(files: &[(&str, &str)]) -> Self {
            let mut dir = Vec::new();
            let mut map = HashMap::new();
            for (name, body) in files {
                dir.push(DirEntry {
                    name: name.to_string(),
                    kind: "file".into(),
                    size: body.len() as u64,
                });
                map.insert(format!("a/b/{name}"), body.as_bytes().to_vec());
            }
            Self {
                commit: Ok(SHA.to_string()),
                files: map,
                dirs: HashMap::from([("a/b".to_string(), dir)]),
            }
        }
    }

    impl RepoReader for FakeRepo {
        fn default_branch_commit(&self, _repo: &str) -> Result<String, String> {
            self.commit.clone()
        }
        fn list_dir(&self, _repo: &str, path: &str, commit: &str) -> Result<Vec<DirEntry>, String> {
            assert_eq!(commit, SHA, "must read at the default branch's commit");
            self.dirs
                .get(path)
                .cloned()
                .ok_or_else(|| "HTTP 404".to_string())
        }
        fn read_file(&self, _repo: &str, path: &str, commit: &str) -> Result<Vec<u8>, String> {
            assert_eq!(commit, SHA, "must read at the default branch's commit");
            self.files
                .get(path)
                .cloned()
                .ok_or_else(|| "HTTP 404".to_string())
        }
    }

    /// Counts what reaches the network behind a cache.
    struct Counting {
        inner: FakeRepo,
        dir_calls: std::cell::Cell<u32>,
        file_calls: std::cell::Cell<u32>,
    }

    impl RepoReader for Counting {
        fn default_branch_commit(&self, repo: &str) -> Result<String, String> {
            self.inner.default_branch_commit(repo)
        }
        fn list_dir(&self, repo: &str, path: &str, commit: &str) -> Result<Vec<DirEntry>, String> {
            self.dir_calls.set(self.dir_calls.get() + 1);
            self.inner.list_dir(repo, path, commit)
        }
        fn read_file(&self, repo: &str, path: &str, commit: &str) -> Result<Vec<u8>, String> {
            self.file_calls.set(self.file_calls.get() + 1);
            self.inner.read_file(repo, path, commit)
        }
    }

    fn counting() -> Counting {
        Counting {
            inner: FakeRepo::with_files(&[("one.json", &approval("one"))]),
            dir_calls: Default::default(),
            file_calls: Default::default(),
        }
    }

    #[test]
    fn a_second_load_of_the_same_commit_makes_no_listing_or_file_request() {
        let d = tempfile::tempdir().unwrap();
        let cache = CachingReader::new(counting(), d.path().to_path_buf());
        let first = load(&source(), &cache);
        assert_eq!((first.handlers.len(), first.error.as_deref()), (1, None));
        assert_eq!(
            (cache.inner.dir_calls.get(), cache.inner.file_calls.get()),
            (1, 1)
        );
        let second = load(&source(), &cache);
        assert_eq!(second.handlers.len(), 1, "{second:?}");
        assert_eq!(
            (cache.inner.dir_calls.get(), cache.inner.file_calls.get()),
            (1, 1),
            "the second load must come from the cache"
        );
    }

    #[test]
    fn the_cache_is_keyed_by_commit_so_a_new_commit_misses() {
        let d = tempfile::tempdir().unwrap();
        let cache = CachingReader::new(counting(), d.path().to_path_buf());
        cache.read_file("o/r", "a/b/one.json", SHA).unwrap();
        let other = "f".repeat(40);
        // The fake refuses any other commit; reaching it proves the cache did not answer.
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = cache.read_file("o/r", "a/b/one.json", &other);
        }));
        assert!(r.is_err(), "a different commit was served from the cache");
    }

    #[test]
    fn a_corrupt_listing_in_the_cache_is_a_miss_and_a_non_sha_commit_is_never_cached() {
        let d = tempfile::tempdir().unwrap();
        let cache = CachingReader::new(counting(), d.path().to_path_buf());
        cache.list_dir("o/r", "a/b", SHA).unwrap();
        for e in std::fs::read_dir(d.path()).unwrap() {
            std::fs::write(e.unwrap().path(), b"{not json").unwrap();
        }
        assert_eq!(cache.list_dir("o/r", "a/b", SHA).unwrap().len(), 1);
        assert_eq!(cache.inner.dir_calls.get(), 2, "a corrupt entry is a miss");
        assert!(cache.entry("file", "o/r", "a/b/x", "main").is_none());
        assert!(cache
            .entry("file", "o/r", "../x", &"a".repeat(40))
            .is_some());
        let name = cache.entry("file", "o/r", "../x", &"a".repeat(40)).unwrap();
        assert_eq!(
            name.parent().unwrap(),
            d.path(),
            "the path never reaches the file name"
        );
    }

    #[test]
    fn a_reader_whose_budget_is_used_up_refuses_without_running_gh() {
        let r = GhReader {
            timeout: Duration::from_secs(15),
            budget: Duration::ZERO,
            started: Instant::now(),
        };
        let e = r.default_branch_commit("o/r").unwrap_err();
        assert!(e.contains("budget"), "{e}");
    }

    fn source() -> ApprovalsSource {
        ApprovalsSource {
            repo: "someone/someone".into(),
            path: "a/b".into(),
        }
    }

    #[test]
    fn loads_every_valid_approval_and_names_its_source_commit() {
        let repo = FakeRepo::with_files(&[
            ("one.json", &approval("one")),
            ("two.json", &approval("two")),
            ("README.md", "not an approval"),
        ]);
        let loaded = load(&source(), &repo);
        assert_eq!(loaded.error, None);
        let ids: Vec<&str> = loaded.handlers.iter().map(|(h, _)| h.id.as_str()).collect();
        assert_eq!(ids, vec!["one", "two"]);
        assert!(loaded.refused.is_empty());
        assert_eq!(
            loaded.source.as_deref(),
            Some("someone/someone:a/b@0123456789ab")
        );
    }

    #[test]
    fn refuses_a_file_whose_name_is_not_its_id() {
        let repo = FakeRepo::with_files(&[("renamed.json", &approval("one"))]);
        let loaded = load(&source(), &repo);
        assert!(loaded.handlers.is_empty());
        assert_eq!(loaded.refused[0].0, "renamed.json");
        assert!(loaded.refused[0].1.contains("one.json"));
    }

    #[test]
    fn one_bad_file_is_refused_without_taking_the_others_down() {
        let secret =
            approval("bad").replace("WARNING: Loading development channels", "Enter password");
        let repo = FakeRepo::with_files(&[("good.json", &approval("good")), ("bad.json", &secret)]);
        let loaded = load(&source(), &repo);
        assert_eq!(loaded.handlers.len(), 1);
        assert_eq!(loaded.handlers[0].0.id, "good");
        assert_eq!(loaded.refused.len(), 1);
        assert!(loaded.refused[0].1.contains("secrets are never approvals"));
    }

    #[test]
    fn a_missing_folder_or_unresolvable_branch_loads_nothing_and_says_why() {
        let mut repo = FakeRepo::with_files(&[("one.json", &approval("one"))]);
        repo.dirs.clear();
        let loaded = load(&source(), &repo);
        assert!(loaded.handlers.is_empty());
        assert!(loaded.error.as_deref().unwrap().contains("404"));

        let mut repo = FakeRepo::with_files(&[("one.json", &approval("one"))]);
        repo.commit = Err("network down".into());
        let loaded = load(&source(), &repo);
        assert!(loaded.handlers.is_empty());
        assert!(loaded.error.as_deref().unwrap().contains("network down"));

        let mut repo = FakeRepo::with_files(&[("one.json", &approval("one"))]);
        repo.commit = Ok("main".into());
        assert!(load(&source(), &repo)
            .error
            .unwrap()
            .contains("not a commit SHA"));
    }

    #[test]
    fn an_oversized_file_is_refused_unread() {
        let mut repo = FakeRepo::with_files(&[("one.json", &approval("one"))]);
        repo.dirs.get_mut("a/b").unwrap()[0].size = MAX_FILE_BYTES + 1;
        repo.files.clear(); // a read would now fail differently
        let loaded = load(&source(), &repo);
        assert!(loaded.refused[0].1.contains("exceeds"));
    }

    #[test]
    fn too_many_files_loads_nothing() {
        let names: Vec<String> = (0..=MAX_FILES).map(|i| format!("h{i}.json")).collect();
        let bodies: Vec<String> = (0..=MAX_FILES)
            .map(|i| approval(&format!("h{i}")))
            .collect();
        let pairs: Vec<(&str, &str)> = names
            .iter()
            .zip(&bodies)
            .map(|(n, b)| (n.as_str(), b.as_str()))
            .collect();
        let loaded = load(&source(), &FakeRepo::with_files(&pairs));
        assert!(loaded.handlers.is_empty());
        assert!(loaded.error.unwrap().contains("limit"));
        // Positive control: exactly MAX_FILES loads.
        let loaded = load(&source(), &FakeRepo::with_files(&pairs[..MAX_FILES]));
        assert_eq!(loaded.handlers.len(), MAX_FILES);
    }

    #[test]
    fn no_config_file_means_not_configured_and_nothing_active() {
        let home = tempfile::tempdir().unwrap();
        let repo = FakeRepo::with_files(&[("one.json", &approval("one"))]);
        let (config, loaded) = load_configured(home.path(), &repo);
        assert!(config.is_none());
        assert!(loaded.handlers.is_empty());
        assert!(loaded
            .error
            .unwrap()
            .contains("no approvals repo configured"));
    }

    #[test]
    fn a_valid_config_is_read_and_its_source_loaded() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".overmind")).unwrap();
        std::fs::write(
            config_path(home.path()),
            r#"{"approvals": {"repo": "someone/someone", "path": "a/b"}}"#,
        )
        .unwrap();
        let repo = FakeRepo::with_files(&[("one.json", &approval("one"))]);
        let (config, loaded) = load_configured(home.path(), &repo);
        assert_eq!(config.unwrap().notify_role, "pm");
        assert_eq!(loaded.handlers.len(), 1);
    }

    #[test]
    fn config_rejects_unsafe_repo_and_path_values_and_unknown_fields() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".overmind")).unwrap();
        for bad in [
            r#"{"approvals": {"repo": "someone", "path": "a"}}"#,
            r#"{"approvals": {"repo": "a/b/c", "path": "a"}}"#,
            r#"{"approvals": {"repo": "a/b", "path": "../x"}}"#,
            r#"{"approvals": {"repo": "a/b", "path": "/abs"}}"#,
            r#"{"approvals": {"repo": "a/b", "path": "a?ref=evil"}}"#,
            r#"{"approvals": {"repo": "a/b", "path": "a"}, "extra": 1}"#,
        ] {
            std::fs::write(config_path(home.path()), bad).unwrap();
            assert!(
                matches!(
                    load_config(&config_path(home.path())),
                    Err(ConfigError::Invalid(_))
                ),
                "{bad}"
            );
        }
        std::fs::write(
            config_path(home.path()),
            r#"{"approvals": {"repo": "ciresnave/ciresnave", "path": ".overmind/lane-restart/approvals"}}"#,
        )
        .unwrap();
        assert!(
            load_config(&config_path(home.path())).is_ok(),
            "positive control"
        );
    }

    #[test]
    fn run_with_timeout_reports_a_missing_program_instead_of_panicking() {
        let err = run_with_timeout(
            "definitely-not-a-real-program-xyz",
            &[],
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(err.contains("could not run"));
    }

    // -- last-known-good approvals (2026-10-22: a slow link made the tip lookup time out and a
    //    lane's dev-channels dialog went unanswered although its approval existed) -------------

    thread_local! {
        static NOW: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }

    fn clock() -> u64 {
        NOW.with(|n| n.get())
    }

    fn set_now(t: u64) {
        NOW.with(|n| n.set(t));
    }

    const HOUR: u64 = 3600;
    const T0: u64 = 1_000_000;

    /// A GitHub that cannot be reached: the tip lookup fails with `error`, and any listing or file
    /// request that gets through to the network fails too, so a handler that loads must have come
    /// from the cache.
    fn down(error: &str) -> FakeRepo {
        let mut r = FakeRepo::with_files(&[]);
        r.commit = Err(error.to_string());
        r.dirs.clear();
        r
    }

    fn cache_over<R: RepoReader>(inner: R, dir: &Path) -> CachingReader<R> {
        CachingReader::with_clock(inner, dir.to_path_buf(), clock)
    }

    /// A cache directory in which one approval has been fully loaded at `T0`.
    fn primed() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        set_now(T0);
        let live = cache_over(
            FakeRepo::with_files(&[("one.json", &approval("one"))]),
            d.path(),
        );
        let first = load(&source(), &live);
        assert_eq!((first.handlers.len(), first.error.as_deref()), (1, None));
        assert_eq!(first.stale, None);
        assert!(
            first.summary().contains("tip read live"),
            "{}",
            first.summary()
        );
        d
    }

    #[test]
    fn a_timed_out_tip_lookup_uses_the_last_fully_loaded_commit_and_says_so() {
        let d = primed();
        set_now(T0 + 3 * HOUR);
        let loaded = load(
            &source(),
            &cache_over(down("gh timed out after 7s"), d.path()),
        );
        assert_eq!(loaded.error, None, "{loaded:?}");
        assert_eq!(
            loaded.handlers.len(),
            1,
            "the approval must load from the cache"
        );
        let note = loaded
            .stale
            .as_deref()
            .expect("a stand-in commit is marked stale");
        assert!(
            note.contains("3h00min") && note.contains("timed out"),
            "{note}"
        );
        assert!(loaded.summary().contains("STALE"), "{}", loaded.summary());
    }

    #[test]
    fn a_remembered_commit_older_than_24_hours_is_not_used() {
        let d = primed();
        set_now(T0 + 24 * HOUR + 60);
        let loaded = load(
            &source(),
            &cache_over(down("gh timed out after 7s"), d.path()),
        );
        assert_eq!(loaded.handlers.len(), 0);
        let e = loaded.error.expect("must fail closed");
        assert!(e.contains("old (limit 24h00min)"), "{e}");
        set_now(T0 + 24 * HOUR);
        let at_limit = load(
            &source(),
            &cache_over(down("gh timed out after 7s"), d.path()),
        );
        assert_eq!(
            at_limit.handlers.len(),
            1,
            "exactly 24 h is still inside the window"
        );
    }

    #[test]
    fn a_stand_in_commit_does_not_refresh_its_own_verified_time() {
        let d = primed();
        set_now(T0 + 20 * HOUR);
        let first = load(
            &source(),
            &cache_over(down("gh timed out after 7s"), d.path()),
        );
        assert_eq!(first.handlers.len(), 1);
        // 25 h after the real verification, 5 h after the stand-in: must be refused.
        set_now(T0 + 25 * HOUR);
        let later = load(
            &source(),
            &cache_over(down("gh timed out after 7s"), d.path()),
        );
        assert_eq!(later.handlers.len(), 0, "{later:?}");
    }

    #[test]
    fn only_transient_failures_fall_back_everything_else_fails_closed() {
        let d = primed();
        set_now(T0 + HOUR);
        for refusal in [
            "HTTP 404: Not Found (https://api.github.com/repos/x/y/commits/HEAD)",
            "HTTP 401: Bad credentials",
            "HTTP 403: API rate limit exceeded",
            "gh: something we have never seen",
            "",
        ] {
            let loaded = load(&source(), &cache_over(down(refusal), d.path()));
            assert_eq!(
                loaded.handlers.len(),
                0,
                "{refusal:?} must not use the remembered commit"
            );
            assert_eq!(loaded.stale, None);
            assert!(loaded.error.is_some(), "{refusal:?}");
        }
    }

    #[test]
    fn is_transient_names_the_network_failures_and_nothing_else() {
        for yes in [
            "gh timed out after 7s",
            "error connecting to api.github.com",
            "Get \"https://api.github.com/\": dial tcp: lookup api.github.com: no such host",
            "read tcp 1.2.3.4:5->6.7.8.9:443: connection reset by peer",
            "net/http: TLS handshake timeout",
            "HTTP 502: Bad Gateway",
            "HTTP 503: Service Unavailable",
        ] {
            assert!(is_transient(yes), "{yes}");
        }
        for no in [
            "HTTP 404: Not Found",
            "HTTP 401: Bad credentials",
            "HTTP 403: Forbidden",
            "HTTP 422: Unprocessable",
            "could not run gh: program not found",
            "",
        ] {
            assert!(!is_transient(no), "{no}");
        }
    }

    #[test]
    fn nothing_remembered_means_nothing_loads() {
        let d = tempfile::tempdir().unwrap();
        set_now(T0);
        let loaded = load(
            &source(),
            &cache_over(down("gh timed out after 7s"), d.path()),
        );
        assert_eq!(loaded.handlers.len(), 0);
        assert!(loaded
            .error
            .unwrap()
            .contains("no remembered approvals commit"));
    }

    #[test]
    fn a_commit_is_remembered_only_after_it_loaded_completely() {
        // The tip resolves but the listing then fails: that commit must not become "last good".
        let d = tempfile::tempdir().unwrap();
        set_now(T0);
        let mut broken = FakeRepo::with_files(&[("one.json", &approval("one"))]);
        broken.dirs.clear();
        let first = load(&source(), &cache_over(broken, d.path()));
        assert!(first.error.is_some());
        set_now(T0 + HOUR);
        let later = load(
            &source(),
            &cache_over(down("gh timed out after 7s"), d.path()),
        );
        assert_eq!(later.handlers.len(), 0);
        assert!(later
            .error
            .unwrap()
            .contains("no remembered approvals commit"));
    }

    #[test]
    fn a_remembered_commit_whose_files_are_not_cached_fails_closed() {
        let d = primed();
        for entry in std::fs::read_dir(d.path()).unwrap() {
            let p = entry.unwrap().path();
            if p.extension().is_some_and(|e| e == "cache") {
                std::fs::remove_file(p).unwrap();
            }
        }
        set_now(T0 + HOUR);
        let loaded = load(
            &source(),
            &cache_over(down("gh timed out after 7s"), d.path()),
        );
        assert_eq!(loaded.handlers.len(), 0, "{loaded:?}");
        assert!(loaded.error.is_some());
    }

    #[test]
    fn a_pointer_for_another_repo_or_with_a_bad_commit_is_ignored() {
        let d = primed();
        let reader = cache_over(down("gh timed out after 7s"), d.path());
        let path = reader.pointer_path(&source().repo);
        for body in [
            format!(r#"{{"repo":"other/repo","commit":"{SHA}","verified_at":{T0}}}"#),
            format!(r#"{{"repo":"someone/someone","commit":"not-a-sha","verified_at":{T0}}}"#),
            "{ not json".to_string(),
        ] {
            std::fs::write(&path, body).unwrap();
            set_now(T0 + HOUR);
            let loaded = load(&source(), &reader);
            assert_eq!(loaded.handlers.len(), 0);
            assert!(loaded.error.is_some());
        }
    }

    // -- one call for the whole folder (2026-10-22: one gh read per file starved the 20 s budget) ------

    /// A reader that can ONLY answer through `snapshot`: any per-file call panics, so a test passing
    /// proves the load used one round trip.
    struct BulkRepo {
        commit: Result<String, String>,
        files: Vec<(String, Option<String>, u64)>,
        calls: std::cell::Cell<u32>,
    }

    impl BulkRepo {
        fn with_approvals(n: usize) -> Self {
            let files = (0..n)
                .map(|i| {
                    let id = format!("approval-{i}");
                    let body = approval(&id);
                    let size = body.len() as u64;
                    (format!("{id}.json"), Some(body), size)
                })
                .collect();
            Self {
                commit: Ok(SHA.to_string()),
                files,
                calls: Default::default(),
            }
        }
    }

    impl RepoReader for BulkRepo {
        fn default_branch_commit(&self, _: &str) -> Result<String, String> {
            panic!("the per-file path was used")
        }
        fn list_dir(&self, _: &str, _: &str, _: &str) -> Result<Vec<DirEntry>, String> {
            panic!("the per-file path was used")
        }
        fn read_file(&self, _: &str, _: &str, _: &str) -> Result<Vec<u8>, String> {
            panic!("the per-file path was used")
        }
        fn snapshot(&self, _: &str, _: &str) -> Option<Result<Snapshot, String>> {
            self.calls.set(self.calls.get() + 1);
            Some(self.commit.clone().map(|commit| {
                Snapshot {
                    commit,
                    entries: self
                        .files
                        .iter()
                        .map(|(name, body, size)| {
                            (
                                DirEntry {
                                    name: name.clone(),
                                    kind: "file".into(),
                                    size: *size,
                                },
                                body.as_ref().map(|b| b.as_bytes().to_vec()),
                            )
                        })
                        .collect(),
                }
            }))
        }
    }

    /// The old shape: only per-file calls, and (like `GhReader`) a budget that runs out after `allowed` of them.
    struct PerFileWithBudget {
        inner: FakeRepo,
        allowed: u32,
        used: std::cell::Cell<u32>,
    }

    impl PerFileWithBudget {
        fn spend(&self) -> Result<(), String> {
            self.used.set(self.used.get() + 1);
            if self.used.get() > self.allowed {
                Err("the 20s approvals budget is used up".into())
            } else {
                Ok(())
            }
        }
    }

    impl RepoReader for PerFileWithBudget {
        fn default_branch_commit(&self, r: &str) -> Result<String, String> {
            self.spend()?;
            self.inner.default_branch_commit(r)
        }
        fn list_dir(&self, r: &str, p: &str, c: &str) -> Result<Vec<DirEntry>, String> {
            self.spend()?;
            self.inner.list_dir(r, p, c)
        }
        fn read_file(&self, r: &str, p: &str, c: &str) -> Result<Vec<u8>, String> {
            self.spend()?;
            self.inner.read_file(r, p, c)
        }
    }

    #[test]
    fn forty_approvals_load_in_one_call_where_one_call_per_file_starves_the_budget() {
        // Control: the per-file shape with a budget of a few calls loads only some of 40.
        let owned: Vec<(String, String)> = (0..40)
            .map(|i| {
                (
                    format!("approval-{i}.json"),
                    approval(&format!("approval-{i}")),
                )
            })
            .collect();
        let borrowed: Vec<(&str, &str)> = owned
            .iter()
            .map(|(n, b)| (n.as_str(), b.as_str()))
            .collect();
        let old = PerFileWithBudget {
            inner: FakeRepo::with_files(&borrowed),
            allowed: 6,
            used: Default::default(),
        };
        let starved = load(&source(), &old);
        assert!(
            starved.handlers.len() < 40,
            "the control must starve: {}",
            starved.summary()
        );
        assert!(!starved.refused.is_empty() || starved.error.is_some());

        // The fix: one snapshot call, all 40, no per-file call (BulkRepo panics on one).
        let bulk = BulkRepo::with_approvals(40);
        let loaded = load(&source(), &bulk);
        assert_eq!(loaded.error, None, "{loaded:?}");
        assert_eq!((loaded.handlers.len(), loaded.refused.len()), (40, 0));
        assert_eq!(bulk.calls.get(), 1);
        assert!(
            loaded.summary().contains("40 active"),
            "{}",
            loaded.summary()
        );
    }

    #[test]
    fn a_snapshot_load_applies_the_same_rules_as_the_per_file_load() {
        let mut bulk = BulkRepo::with_approvals(2);
        bulk.files
            .push(("README.md".into(), Some("not an approval".into()), 15));
        bulk.files.push((
            "wrong-name.json".into(),
            Some(approval("something-else")),
            100,
        ));
        bulk.files
            .push(("huge.json".into(), None, MAX_FILE_BYTES + 1));
        bulk.files.push(("not-delivered.json".into(), None, 100));
        bulk.files
            .push(("bad.json".into(), Some("{ not json".into()), 10));
        let loaded = load(&source(), &bulk);
        assert_eq!(loaded.handlers.len(), 2);
        let refused: std::collections::HashMap<_, _> = loaded.refused.iter().cloned().collect();
        assert!(
            refused["wrong-name.json"].contains("file name must be"),
            "{refused:?}"
        );
        assert!(refused["huge.json"].contains("exceeds"), "{refused:?}");
        assert!(
            refused["not-delivered.json"].contains("not delivered"),
            "{refused:?}"
        );
        assert!(refused.contains_key("bad.json") && !refused.contains_key("README.md"));
        // Too many files, and a tip that is not a SHA, are still refused.
        assert!(load(&source(), &BulkRepo::with_approvals(MAX_FILES + 1))
            .error
            .unwrap()
            .contains("exceeds the limit"));
        let mut odd = BulkRepo::with_approvals(1);
        odd.commit = Ok("main".into());
        assert!(load(&source(), &odd)
            .error
            .unwrap()
            .contains("not a commit SHA"));
        let mut down = BulkRepo::with_approvals(1);
        down.commit = Err("HTTP 404".into());
        assert!(load(&source(), &down)
            .error
            .unwrap()
            .contains("could not resolve"));
    }

    #[test]
    fn a_snapshot_that_timed_out_falls_back_to_the_cache_the_last_good_snapshot_filled() {
        let d = tempfile::tempdir().unwrap();
        set_now(T0);
        let live = cache_over(BulkRepo::with_approvals(40), d.path());
        assert_eq!(load(&source(), &live).handlers.len(), 40);

        let mut gone = BulkRepo::with_approvals(0);
        gone.commit = Err("gh timed out after 15s".into());
        set_now(T0 + 2 * HOUR);
        let stale = load(&source(), &cache_over(gone, d.path()));
        assert_eq!(
            (stale.handlers.len(), stale.error.as_deref()),
            (40, None),
            "{stale:?}"
        );
        assert!(
            stale
                .stale
                .as_deref()
                .is_some_and(|n| n.contains("2h00min")),
            "{:?}",
            stale.stale
        );

        for (when, error, why) in [
            (T0 + 25 * HOUR, "gh timed out after 15s", "older than 24 h"),
            (T0 + HOUR, "HTTP 403: rate limit", "not transient"),
        ] {
            set_now(when);
            let mut b = BulkRepo::with_approvals(0);
            b.commit = Err(error.into());
            let l = load(&source(), &cache_over(b, d.path()));
            assert_eq!(l.handlers.len(), 0, "{why}: {l:?}");
            assert!(l.error.is_some(), "{why}");
        }
        // Nothing cached for the remembered commit: fail closed.
        for e in std::fs::read_dir(d.path()).unwrap() {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "cache") {
                std::fs::remove_file(p).unwrap();
            }
        }
        set_now(T0 + HOUR);
        let mut b = BulkRepo::with_approvals(0);
        b.commit = Err("gh timed out after 15s".into());
        let l = load(&source(), &cache_over(b, d.path()));
        assert_eq!(l.handlers.len(), 0);
        assert!(l.error.unwrap().contains("not cached"));
    }

    #[test]
    fn parse_snapshot_reads_the_graphql_answer_and_refuses_the_rest() {
        let ok = serde_json::json!({"data": {"repository": {
        "head": {"oid": SHA},
        "dir": {"entries": [
            {"name": "a.json", "type": "blob", "object": {"byteSize": 5, "isTruncated": false, "isBinary": false, "text": "hello"}},
            {"name": "big.json", "type": "blob", "object": {"byteSize": 999999, "isTruncated": true, "isBinary": false, "text": "he"}},
            {"name": "img.json", "type": "blob", "object": {"byteSize": 3, "isTruncated": false, "isBinary": true, "text": null}},
            {"name": "sub", "type": "tree", "object": {}}
        ]}}}});
        let snap = parse_snapshot(ok.to_string().as_bytes(), "a/b").unwrap();
        assert_eq!(snap.commit, SHA);
        let by: std::collections::HashMap<_, _> = snap
            .entries
            .iter()
            .map(|(e, b)| (e.name.as_str(), (e.kind.as_str(), e.size, b.clone())))
            .collect();
        assert_eq!(by["a.json"], ("file", 5, Some(b"hello".to_vec())));
        assert_eq!(by["big.json"].2, None, "a truncated blob has no body");
        assert_eq!(by["img.json"].2, None, "a binary blob has no body");
        assert_eq!(by["sub"].0, "tree");

        let not_folder =
            serde_json::json!({"data": {"repository": {"head": {"oid": SHA}, "dir": null}}});
        assert!(parse_snapshot(not_folder.to_string().as_bytes(), "a/b")
            .unwrap_err()
            .contains("not a folder"));
        let a_file = serde_json::json!({"data": {"repository": {"head": {"oid": SHA}, "dir": {}}}});
        assert!(parse_snapshot(a_file.to_string().as_bytes(), "a/b").is_err());
        let no_repo = serde_json::json!({"data": {"repository": null}, "errors": [{"message": "Could not resolve to a Repository"}]});
        assert!(parse_snapshot(no_repo.to_string().as_bytes(), "a/b")
            .unwrap_err()
            .contains("Could not resolve"));
        let no_tip =
            serde_json::json!({"data": {"repository": {"head": null, "dir": {"entries": []}}}});
        assert!(parse_snapshot(no_tip.to_string().as_bytes(), "a/b").is_err());
        assert!(parse_snapshot(b"<html>", "a/b").is_err());
    }
}
