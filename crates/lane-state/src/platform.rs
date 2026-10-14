// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where the portfolio's fixed directories live on this OS (`docs/PORT-LINUX-PLAN.md`, slice P0).
//!
//! On Windows every value is the literal it has always been (`C:/Projects/...`), so nothing that
//! runs there changes. Off Windows those literals are relative paths (`C:/Projects` is a directory
//! *named* `C:` under the cwd), so each resolves under the XDG base directories instead.
//!
//! The decision is a pure function of [`Env`], so the non-Windows branch is tested on a Windows
//! machine too. None of these is configurable by a command-line flag: the state directory is "one
//! fixed place every lane and the PM agree on" (RESTART-TOOL-DESIGN.md section 7).

use std::path::PathBuf;

/// What the resolvers read, passed in so a test can stand in either OS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Env {
    pub windows: bool,
    /// `XDG_STATE_HOME`; ignored unless absolute, as the XDG specification says.
    pub xdg_state_home: Option<PathBuf>,
    /// `HOME` (or `USERPROFILE`).
    pub home: Option<PathBuf>,
}

impl Env {
    /// This process's environment.
    pub fn current() -> Env {
        Env {
            windows: cfg!(windows),
            xdg_state_home: std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
            home: crate::paths::home_dir(),
        }
    }

    fn state_base(&self) -> Option<PathBuf> {
        match &self.xdg_state_home {
            Some(p) if is_absolute(p) => Some(p.clone()),
            _ => self.home.as_ref().map(|h| h.join(".local").join("state")),
        }
    }
}

/// Absolute on either OS's rules: a leading `/` counts on Windows too, so the XDG rule reads the
/// same wherever it is tested.
fn is_absolute(p: &std::path::Path) -> bool {
    p.is_absolute() || p.to_string_lossy().starts_with('/')
}

pub const WINDOWS_PORTFOLIO_ROOT: &str = "C:/Projects";

/// The directory every lane's checkout lives under.
pub fn try_portfolio_root(env: &Env) -> Option<PathBuf> {
    if env.windows {
        Some(PathBuf::from(WINDOWS_PORTFOLIO_ROOT))
    } else {
        env.home.as_ref().map(|h| h.join("Projects"))
    }
}

/// `lane-restart`'s and the hooks' shared state directory (state files, launch specs, the ledger).
pub fn try_state_dir(env: &Env) -> Option<PathBuf> {
    if env.windows {
        Some(PathBuf::from("C:/Projects/.lane-state"))
    } else {
        env.state_base().map(|b| b.join("lane-state"))
    }
}

/// AgentLife's own home (registry, journal, pending requests, plans, reports). Never inside the
/// state directory (`Home::new` refuses it).
pub fn try_agentlife_home(env: &Env) -> Option<PathBuf> {
    if env.windows {
        Some(PathBuf::from("C:/Projects/.agentlife"))
    } else {
        env.state_base().map(|b| b.join("agentlife"))
    }
}

/// Where the installed `lane-restart` and `agentlife` executables live.
pub fn try_hooks_dir(env: &Env) -> Option<PathBuf> {
    if env.windows {
        Some(PathBuf::from("C:/Projects/.claude-hooks"))
    } else {
        env.home
            .as_ref()
            .map(|h| h.join(".local").join("lib").join("agentlife").join("hooks"))
    }
}

/// A directory under the temp dir, for a machine with no usable home. It is *a* directory rather
/// than a relative one, and it is named for what it replaces; it is not a place to keep anything.
fn fallback(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("agentlife-{name}"))
}

pub fn portfolio_root() -> PathBuf {
    try_portfolio_root(&Env::current()).unwrap_or_else(|| fallback("projects"))
}

pub fn state_dir() -> PathBuf {
    try_state_dir(&Env::current()).unwrap_or_else(|| fallback("lane-state"))
}

pub fn agentlife_home() -> PathBuf {
    try_agentlife_home(&Env::current()).unwrap_or_else(|| fallback("home"))
}

pub fn hooks_dir() -> PathBuf {
    try_hooks_dir(&Env::current()).unwrap_or_else(|| fallback("hooks"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win() -> Env {
        Env {
            windows: true,
            xdg_state_home: Some(PathBuf::from("/ignored")),
            home: Some(PathBuf::from("C:/Users/x")),
        }
    }

    /// A resolved path as a `/`-separated string, so Linux values compare the same on Windows.
    fn n(p: Option<PathBuf>) -> Option<String> {
        p.map(|p| p.to_string_lossy().replace('\\', "/"))
    }

    fn linux(xdg: Option<&str>, home: Option<&str>) -> Env {
        Env {
            windows: false,
            xdg_state_home: xdg.map(PathBuf::from),
            home: home.map(PathBuf::from),
        }
    }

    /// Windows keeps today's literals whatever the XDG and home variables say.
    #[test]
    fn windows_values_are_the_historical_literals() {
        let e = win();
        assert_eq!(n(try_portfolio_root(&e)), Some(String::from("C:/Projects")));
        assert_eq!(
            n(try_state_dir(&e)),
            Some(String::from("C:/Projects/.lane-state"))
        );
        assert_eq!(
            n(try_agentlife_home(&e)),
            Some(String::from("C:/Projects/.agentlife"))
        );
        assert_eq!(
            n(try_hooks_dir(&e)),
            Some(String::from("C:/Projects/.claude-hooks"))
        );
    }

    #[test]
    fn linux_uses_xdg_state_home_when_it_is_absolute() {
        let e = linux(Some("/var/s"), Some("/home/u"));
        assert_eq!(
            n(try_state_dir(&e)),
            Some(String::from("/var/s/lane-state"))
        );
        assert_eq!(
            n(try_agentlife_home(&e)),
            Some(String::from("/var/s/agentlife"))
        );
    }

    #[test]
    fn linux_falls_back_to_home_local_state_and_ignores_a_relative_xdg() {
        for xdg in [None, Some("relative/dir"), Some("")] {
            let e = linux(xdg, Some("/home/u"));
            assert_eq!(
                n(try_state_dir(&e)),
                Some(String::from("/home/u/.local/state/lane-state")),
                "xdg={xdg:?}"
            );
        }
        let e = linux(None, Some("/home/u"));
        assert_eq!(
            n(try_portfolio_root(&e)),
            Some(String::from("/home/u/Projects"))
        );
        assert_eq!(
            n(try_hooks_dir(&e)),
            Some(String::from("/home/u/.local/lib/agentlife/hooks"))
        );
    }

    /// The bug being fixed: nothing a Linux resolver returns is the relative `C:/...` directory.
    #[test]
    fn no_linux_value_is_a_drive_letter_path_and_all_are_absolute() {
        let e = linux(None, Some("/home/u"));
        for p in [
            n(try_portfolio_root(&e)),
            n(try_state_dir(&e)),
            n(try_agentlife_home(&e)),
            n(try_hooks_dir(&e)),
        ] {
            let p = p.unwrap();
            assert!(p.starts_with('/'), "{p:?}");
            assert!(!p.starts_with("C:"), "{p:?}");
        }
    }

    #[test]
    fn linux_with_no_home_and_no_xdg_resolves_to_nothing_rather_than_a_guess() {
        let e = linux(None, None);
        assert_eq!(n(try_state_dir(&e)), None);
        assert_eq!(n(try_hooks_dir(&e)), None);
    }

    /// The agentlife home is never the state directory or inside it (`Home::new` refuses that).
    #[test]
    fn the_home_is_not_the_state_dir_on_either_os() {
        for e in [win(), linux(None, Some("/home/u"))] {
            let h = n(try_agentlife_home(&e)).unwrap();
            let s = n(try_state_dir(&e)).unwrap();
            assert!(!h.starts_with(&s) && h != s, "{h:?} vs {s:?}");
        }
    }

    #[test]
    fn the_infallible_wrappers_never_return_a_relative_path() {
        // whatever this machine's environment, the wrapper returns an absolute path
        assert!(state_dir().is_absolute());
        assert!(agentlife_home().is_absolute());
    }
}
