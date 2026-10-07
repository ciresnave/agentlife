// SPDX-License-Identifier: MIT OR Apache-2.0
//! Where agentlife keeps its state, and the guarantee that a test can never touch the real one.
//!
//! Default `C:/Projects/.agentlife` (DESIGN-REVISION-1 §2.3), outside every repo. The
//! `AGENTLIFE_HOME` environment variable overrides it, and every test uses a temporary
//! directory through that override. `Home::new` also **refuses any path that contains a
//! `.lane-state` component**: that directory is `lane-restart`'s live runtime state, and
//! agentlife only ever reads it (DESIGN.md §8, coexistence rules).

use std::fmt;
use std::path::{Path, PathBuf};

pub const HOME_ENV: &str = "AGENTLIFE_HOME";
pub const DEFAULT_HOME: &str = "C:/Projects/.agentlife";
const FORBIDDEN_COMPONENT: &str = ".lane-state";

#[derive(Debug, PartialEq, Eq)]
pub enum HomeError {
    /// The home (or a parent of it) is `lane-restart`'s state directory.
    InsideLaneState(PathBuf),
    /// An empty path is never a home.
    Empty,
}

impl fmt::Display for HomeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HomeError::InsideLaneState(p) => write!(
                f,
                "refusing {}: it is inside .lane-state, which agentlife only reads",
                p.display()
            ),
            HomeError::Empty => write!(f, "refusing an empty agentlife home"),
        }
    }
}

impl std::error::Error for HomeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Home(PathBuf);

impl Home {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, HomeError> {
        let path = path.into();
        if path.as_os_str().is_empty() {
            return Err(HomeError::Empty);
        }
        // Split on BOTH separators by hand: on Linux a backslash is an ordinary character, so
        // `Path::components` would miss `C:\Projects\.lane-state` there.
        let forbidden = path
            .to_string_lossy()
            .split(['/', '\\'])
            .any(|part| part.eq_ignore_ascii_case(FORBIDDEN_COMPONENT));
        if forbidden {
            return Err(HomeError::InsideLaneState(path));
        }
        Ok(Self(path))
    }

    /// The home named by `env_value` (the value of `AGENTLIFE_HOME`, if set and non-empty),
    /// else the default.
    pub fn resolve(env_value: Option<&str>) -> Result<Self, HomeError> {
        match env_value {
            Some(v) if !v.is_empty() => Self::new(v),
            _ => Self::new(DEFAULT_HOME),
        }
    }

    /// Reads `AGENTLIFE_HOME` from the process environment.
    pub fn from_env() -> Result<Self, HomeError> {
        Self::resolve(std::env::var(HOME_ENV).ok().as_deref())
    }

    pub fn root(&self) -> &Path {
        &self.0
    }

    pub fn config_file(&self) -> PathBuf {
        self.0.join("config.json")
    }

    pub fn agents_dir(&self) -> PathBuf {
        self.0.join("registry").join("agents")
    }

    pub fn journal_dir(&self) -> PathBuf {
        self.0.join("registry")
    }

    /// `<pid>-<process start>.id` pointer files: process -> agent, so a hook finds its agent
    /// without scanning the fleet (SessionEnd has a 1.5 s budget).
    pub fn procs_dir(&self) -> PathBuf {
        self.0.join("registry").join("procs")
    }

    /// What the hook did or declined to do, one line per event. A hook that fails is silent to
    /// Claude Code, so this file is the only place a broken install shows up.
    pub fn hook_log(&self) -> PathBuf {
        self.0.join("hook.log")
    }

    pub fn pending_dir(&self) -> PathBuf {
        self.0.join("pending")
    }

    /// Frozen restore plans, one file each (`plan::freeze`).
    pub fn plans_dir(&self) -> PathBuf {
        self.0.join("plans")
    }

    pub fn reports_dir(&self) -> PathBuf {
        self.0.join("reports")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_outside_every_repo_and_is_not_lane_state() {
        let h = Home::resolve(None).unwrap();
        assert_eq!(h.root(), Path::new("C:/Projects/.agentlife"));
        assert!(Home::new(DEFAULT_HOME).is_ok());
    }

    #[test]
    fn an_empty_override_means_the_default_not_the_current_directory() {
        assert_eq!(
            Home::resolve(Some("")).unwrap(),
            Home::resolve(None).unwrap()
        );
    }

    #[test]
    fn an_override_wins() {
        let h = Home::resolve(Some("D:/scratch/x")).unwrap();
        assert_eq!(h.root(), Path::new("D:/scratch/x"));
    }

    #[test]
    fn lane_state_is_refused_in_every_spelling() {
        for bad in [
            "C:/Projects/.lane-state",
            "C:/Projects/.lane-state/agentlife",
            "C:\\Projects\\.LANE-STATE\\x",
            ".lane-state",
        ] {
            assert!(
                matches!(Home::new(bad), Err(HomeError::InsideLaneState(_))),
                "{bad} should be refused"
            );
        }
    }

    #[test]
    fn a_name_that_merely_contains_lane_state_is_allowed() {
        // Positive control for the refusal above: it matches a whole component, not a substring.
        assert!(Home::new("C:/Projects/not.lane-state.backup/x").is_ok());
        assert!(Home::new("C:/Projects/lane-state").is_ok());
    }

    #[test]
    fn an_empty_path_is_refused() {
        assert_eq!(Home::new(""), Err(HomeError::Empty));
    }

    #[test]
    fn subpaths_hang_off_the_root() {
        let h = Home::new("X:/h").unwrap();
        assert_eq!(h.config_file(), Path::new("X:/h/config.json"));
        assert_eq!(h.agents_dir(), Path::new("X:/h/registry/agents"));
        assert_eq!(h.journal_dir(), Path::new("X:/h/registry"));
        assert_eq!(h.procs_dir(), Path::new("X:/h/registry/procs"));
        assert_eq!(h.hook_log(), Path::new("X:/h/hook.log"));
        assert_eq!(h.pending_dir(), Path::new("X:/h/pending"));
        assert_eq!(h.reports_dir(), Path::new("X:/h/reports"));
    }
}
