// SPDX-License-Identifier: MIT OR Apache-2.0

//! Path comparison, normalised. RESTART-TOOL-DESIGN.md §2.
//!
//! ⚠️ PM finding, 2026-09-18 (real restart attempt, second retest): a real
//! Windows process's own `cwd` carries a trailing separator
//! (`C:\Projects\.restart-test\`); the hook's own `cwd` in the state file
//! does not (`C:\Projects\.restart-test`). A byte-for-byte `==` compare
//! refused every real identity check, correctly failing closed but for a
//! reason that had nothing to do with the pid's actual identity. This
//! module normalises BOTH sides the same way before comparing: unify `/`
//! and `\`, strip a trailing separator (except a bare drive root, where it
//! is significant - `C:\` and `C:` are not the same path), and compare
//! case-insensitively on Windows, where the filesystem itself is
//! case-insensitive. ⚠️ Never a prefix/`starts_with` match - `C:\a` and
//! `C:\ab` must stay distinct, or a state file for one lane could pass the
//! identity check for another lane whose path happens to start the same.

/// Unifies separators and strips one trailing separator, except from a bare
/// drive root (`C:/`, kept - `C:` alone means something different: that
/// drive's current directory, not its filesystem root).
fn normalize_separators(p: &str) -> String {
    let unified: String = p.chars().map(|c| if c == '\\' { '/' } else { c }).collect();
    if unified.len() > 1 && unified.ends_with('/') {
        let without_trailing = &unified[..unified.len() - 1];
        if without_trailing.ends_with(':') {
            unified
        } else {
            without_trailing.to_string()
        }
    } else {
        unified
    }
}

/// The one place a path is judged "the same" as another for identity
/// purposes - both `cwd` (`authorize::identify`) and `exe`
/// (`facts::kill_verified`) go through this, never a raw `==` on the
/// original strings.
pub fn paths_match(a: &str, b: &str) -> bool {
    let (a, b) = (normalize_separators(a), normalize_separators(b));
    #[cfg(windows)]
    {
        a.eq_ignore_ascii_case(&b)
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trailing_separator_does_not_break_the_match() {
        assert!(paths_match(r"C:\a\", r"C:\a"));
    }

    #[test]
    fn forward_and_back_slashes_match() {
        assert!(paths_match("C:/a", r"C:\a"));
    }

    #[cfg(windows)]
    #[test]
    fn case_differs_but_still_matches_on_windows() {
        assert!(paths_match(r"c:\A", r"C:\a"));
    }

    #[test]
    fn a_longer_sibling_path_is_never_treated_as_the_same_one() {
        // ⚠️ THE PREFIX-MATCH TRAP: normalising must never turn into a
        // `starts_with` check, or C:\a's identity check would also accept
        // a process actually running in C:\ab.
        assert!(!paths_match(r"C:\a", r"C:\ab"));
    }

    #[test]
    fn an_unrelated_path_never_matches() {
        assert!(!paths_match(r"C:\a", r"C:\b"));
    }

    #[test]
    fn a_bare_drive_root_with_its_slash_is_distinct_from_the_drive_letter_alone() {
        assert!(!paths_match(r"C:\", "C:"));
    }
}
