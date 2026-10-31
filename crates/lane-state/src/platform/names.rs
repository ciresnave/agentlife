// SPDX-License-Identifier: MIT OR Apache-2.0

//! Process image names, in one place (`docs/PORT-LINUX-PLAN.md`, slice P2).
//!
//! Five modules each kept their own copy of "is this `claude`" and "is this a shell", and the
//! copies had drifted (one list had `zsh` and `fish`, the ancestry walk did not, so a user on zsh
//! had every hook refused). Everything compares through [`image_base`].
//!
//! ⚠️ What Claude Code's process is called on Linux has not been measured (the plan's section 8,
//! item 1). Until it is, [`is_claude`] accepts exactly the names it always did (`claude`,
//! `claude.exe`); widening it is one change here.

/// The shells a hook or a restart may sit under, lowercase, no extension. Windows and POSIX
/// together: a name that does not exist on an OS never matches there.
pub const SHELLS: &[&str] = &[
    "bash",
    "sh",
    "dash",
    "zsh",
    "fish",
    "ksh",
    "cmd",
    "powershell",
    "pwsh",
];

/// A process name reduced for comparison: the last path component, lowercase, without `.exe`.
pub fn image_base(name: &str) -> String {
    let last = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .to_lowercase();
    last.strip_suffix(".exe").unwrap_or(&last).to_string()
}

/// The Claude Code process.
pub fn is_claude(name: &str) -> bool {
    image_base(name) == "claude"
}

/// A shell from [`SHELLS`].
pub fn is_shell(name: &str) -> bool {
    let b = image_base(name);
    SHELLS.contains(&b.as_str())
}

/// This tool's own executable. A `lane-restart` command that starts another `lane-restart` command
/// (`lane-new` runs `start` as a child) sits between the child and the `claude` session that asked.
pub fn is_lane_restart(name: &str) -> bool {
    image_base(name) == "lane-restart"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_base_drops_the_directory_the_case_and_the_exe() {
        assert_eq!(image_base("C:\\x\\WindowsTerminal.exe"), "windowsterminal");
        assert_eq!(image_base("/usr/bin/Zsh"), "zsh");
        assert_eq!(image_base("claude"), "claude");
    }

    #[test]
    fn claude_is_claude_with_or_without_exe_and_nothing_else() {
        assert!(is_claude("claude") && is_claude("Claude.EXE") && is_claude("/opt/bin/claude"));
        assert!(!is_claude("claude-code") && !is_claude("node") && !is_claude("claudex"));
    }

    /// The bug this slice fixes: zsh, fish and dash were refused as ancestors.
    #[test]
    fn posix_shells_are_shells_and_a_terminal_is_not() {
        for s in [
            "bash",
            "sh",
            "dash",
            "zsh",
            "fish",
            "ksh",
            "cmd.exe",
            "pwsh",
            "PowerShell.exe",
        ] {
            assert!(is_shell(s), "{s}");
        }
        for s in [
            "windowsterminal",
            "gnome-terminal-server",
            "claude",
            "node",
            "",
        ] {
            assert!(!is_shell(s), "{s}");
        }
    }
}
