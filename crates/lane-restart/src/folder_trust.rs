// SPDX-License-Identifier: MIT OR Apache-2.0

//! The folder-trust prefix approval (board 244, CireSnave 2026-10-22: *"Blanket trust all directories
//! under C:\Projects so future projects are already approved for that dialog."*).
//!
//! One approval record kind, `match.folder_prefix`, answers Claude Code's **folder-trust dialog and
//! nothing else** for any folder strictly under a prefix. The knowingly accepted risk is that anything
//! able to create a folder under the prefix gets trusted; everything here exists so that is the ONLY
//! way in. A path is trusted only if all of these hold, otherwise nothing is answered:
//!
//! 1. it parses as a plain absolute drive path (`X:\a\b`): no UNC or `\\?\` form, no `.`/`..`/empty
//!    segment, no `/`, no 8.3 short name (`LONGNA~1`), no trailing dot or space, no reserved character;
//! 2. component by component (never a string prefix, so `C:\Projects-evil` and `C:\ProjectsX` fail) it is
//!    strictly under the prefix, compared case-insensitively;
//! 3. the filesystem agrees: its canonical form (junctions, symlinks and `subst` followed) EQUALS what the
//!    dialog showed and is still strictly under the prefix, and the prefix itself is not redirected;
//! 4. the dialog showed it on one row followed by a blank row (a hard-wrapped long path never passes).
//!
//! The filesystem is reached through [`FolderResolver`] so tests can stage junctions and short names.

use std::path::Path;

/// Resolves a path to its canonical form, as the filesystem sees it. `None` if it does not exist or
/// cannot be resolved (which refuses it: fail closed).
pub trait FolderResolver {
    fn canonical(&self, path: &str) -> Option<String>;
}

/// The real filesystem.
pub struct FsResolver;

impl FolderResolver for FsResolver {
    fn canonical(&self, path: &str) -> Option<String> {
        let resolved = std::fs::canonicalize(Path::new(path)).ok()?;
        let text = resolved.to_str()?;
        // Windows canonical form is `\\?\C:\x`; `\\?\UNC\...` is deliberately left alone (it is refused).
        Some(
            text.strip_prefix(r"\\?\")
                .filter(|rest| !rest.starts_with("UNC\\"))
                .unwrap_or(text)
                .to_owned(),
        )
    }
}

/// The drive letter (uppercased) and components of a plain absolute drive path, or `None` for
/// anything else (see rule 1 above).
pub fn split_path(path: &str) -> Option<(char, Vec<&str>)> {
    let bytes = path.as_bytes();
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' || bytes[2] != b'\\' {
        return None;
    }
    let comps: Vec<&str> = path[3..].split('\\').collect();
    if comps.iter().any(|c| !valid_component(c)) {
        return None;
    }
    Some((bytes[0].to_ascii_uppercase() as char, comps))
}

fn valid_component(c: &str) -> bool {
    if c.is_empty() || c == "." || c == ".." || c.ends_with('.') || c.ends_with(' ') {
        return false;
    }
    if c.chars()
        .any(|ch| ch.is_control() || "<>:\"/|?*".contains(ch))
    {
        return false;
    }
    // An 8.3 short name has `~` followed by a digit (`PROGRA~1`).
    let b = c.as_bytes();
    !b.windows(2).any(|w| w[0] == b'~' && w[1].is_ascii_digit())
}

fn lower(comps: &[&str]) -> Vec<String> {
    comps.iter().map(|c| c.to_lowercase()).collect()
}

/// Is `path` strictly below `prefix` (component-wise, case-insensitive, same drive)? Both must be
/// plain drive paths.
pub fn is_strictly_under(prefix: &str, path: &str) -> bool {
    let (Some((pd, pc)), Some((d, c))) = (split_path(prefix), split_path(path)) else {
        return false;
    };
    let (pc, c) = (lower(&pc), lower(&c));
    pd == d && c.len() > pc.len() && c[..pc.len()] == pc[..]
}

fn same_path(a: &str, b: &str) -> bool {
    match (split_path(a), split_path(b)) {
        (Some((da, ca)), Some((db, cb))) => da == db && lower(&ca) == lower(&cb),
        _ => false,
    }
}

/// The canonical path of `shown` if it may be trusted under `prefix`, else `None`.
pub fn trusted_folder(prefix: &str, shown: &str, resolver: &dyn FolderResolver) -> Option<String> {
    if !is_strictly_under(prefix, shown) {
        return None;
    }
    // The prefix itself must not be a redirect (it would move every folder under it).
    if !resolver
        .canonical(prefix)
        .is_some_and(|p| same_path(&p, prefix))
    {
        return None;
    }
    let canonical = resolver.canonical(shown)?;
    // The filesystem must say the same thing the dialog did: a junction, symlink, `subst` or any
    // other alias makes them differ, and an unparseable canonical form (UNC) refuses outright.
    if !same_path(&canonical, shown) || !is_strictly_under(prefix, &canonical) {
        return None;
    }
    Some(canonical)
}

/// The folder a folder-trust dialog is asking about: the first non-blank row after the single row
/// `Accessing workspace:`, trimmed, provided the row after it is blank or absent (a wrapped path
/// continues on that row and is refused).
pub fn dialog_folder(screen_text: &str) -> Option<String> {
    let rows: Vec<&str> = screen_text.lines().collect();
    let mut headers = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.trim() == "Accessing workspace:");
    let (at, _) = headers.next()?;
    if headers.next().is_some() {
        return None;
    }
    let (k, row) = rows
        .iter()
        .enumerate()
        .skip(at + 1)
        .take(3)
        .find(|(_, r)| !r.trim().is_empty())?;
    if rows.get(k + 1).is_some_and(|next| !next.trim().is_empty()) {
        return None;
    }
    Some(row.trim().to_owned())
}

/// A staged filesystem for tests here and in `handlers`/`host`: `(path lowercased) -> canonical form`.
/// A path absent from the map does not exist; `C:\Projects` always does.
#[cfg(test)]
pub(crate) mod staged {
    use super::FolderResolver;
    use std::collections::HashMap;

    pub struct StagedFs(HashMap<String, String>);

    impl StagedFs {
        pub fn with(dirs: &[&str]) -> StagedFs {
            let mut m: HashMap<String, String> = dirs
                .iter()
                .map(|d| (d.to_lowercase(), (*d).to_owned()))
                .collect();
            m.insert(r"c:\projects".to_owned(), r"C:\Projects".to_owned());
            StagedFs(m)
        }

        pub fn alias(mut self, path: &str, canonical: &str) -> StagedFs {
            self.0.insert(path.to_lowercase(), canonical.to_owned());
            self
        }
    }

    impl FolderResolver for StagedFs {
        fn canonical(&self, path: &str) -> Option<String> {
            self.0.get(&path.to_lowercase()).cloned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const PREFIX: &str = r"C:\Projects";

    use super::staged::StagedFs as Fake;

    fn trusted(shown: &str, fs: &Fake) -> bool {
        trusted_folder(PREFIX, shown, fs).is_some()
    }

    #[test]
    fn a_real_folder_strictly_under_the_prefix_is_trusted_and_reported_in_canonical_form() {
        let fs = Fake::with(&[r"C:\Projects\antspawn", r"C:\Projects\a\b"]);
        assert_eq!(
            trusted_folder(PREFIX, r"C:\Projects\antspawn", &fs).as_deref(),
            Some(r"C:\Projects\antspawn")
        );
        assert!(
            trusted(r"C:\Projects\a\b", &fs),
            "nested folders are under the prefix too"
        );
        assert!(
            trusted(r"c:\PROJECTS\ANTSPAWN", &fs),
            "NTFS paths compare case-insensitively"
        );
    }

    #[test]
    fn the_prefix_itself_and_other_drives_are_not_trusted() {
        let fs = Fake::with(&[r"C:\Projects\x", r"D:\Projects\x"]);
        assert!(!trusted(PREFIX, &fs));
        assert!(
            !trusted(r"C:\Projects\", &fs),
            "trailing backslash is an empty component"
        );
        assert!(!trusted(r"D:\Projects\x", &fs));
        assert!(!trusted(r"C:\", &fs));
    }

    #[test]
    fn prefix_confusion_is_refused() {
        let fs = Fake::with(&[
            r"C:\Projects-evil\x",
            r"C:\ProjectsX\y",
            r"C:\Projects.old\z",
            r"C:\Project\q",
        ]);
        for p in [
            r"C:\Projects-evil\x",
            r"C:\ProjectsX\y",
            r"C:\Projects.old\z",
            r"C:\Project\q",
        ] {
            assert!(!trusted(p, &fs), "{p}");
        }
        assert!(
            !is_strictly_under(PREFIX, r"C:\Projectsx"),
            "a string prefix is not a path prefix"
        );
    }

    #[test]
    fn dot_dot_unc_short_names_and_odd_characters_are_refused() {
        let fs = Fake::with(&[
            r"C:\Projects\ok",
            r"C:\Windows",
            r"C:\Projects\LONGNA~1",
            r"C:\Projects\ok.",
            r"C:\Projects\ok ",
        ]);
        for p in [
            r"C:\Projects\..\Windows",
            r"C:\Projects\ok\..\..\Windows",
            r"C:\Projects\.\ok",
            r"\\server\share\Projects\ok",
            r"\\?\C:\Projects\ok",
            r"\\?\UNC\server\share\ok",
            r"C:/Projects/ok",
            r"C:\Projects\LONGNA~1",
            r"C:\Projects\ok.",
            r"C:\Projects\ok ",
            r"C:\Projects\ok:stream",
            r"C:\Projects\a*b",
            r"C:\Projects\\ok",
            r"Projects\ok",
            "",
        ] {
            assert!(!trusted(p, &fs), "{p:?}");
        }
    }

    #[test]
    fn a_junction_or_symlink_out_of_the_prefix_is_refused_after_resolving() {
        // `C:\Projects\evil` is a junction to `D:\secrets`; the dialog shows the junction's own path.
        let fs = Fake::with(&[r"C:\Projects\good"])
            .alias(r"C:\Projects\evil", r"D:\secrets")
            .alias(r"C:\Projects\evil2", r"C:\Windows\System32")
            .alias(r"C:\Projects\sneaky", r"C:\Projects-evil\x");
        assert!(
            trusted(r"C:\Projects\good", &fs),
            "control: the same fake accepts a real folder"
        );
        for p in [
            r"C:\Projects\evil",
            r"C:\Projects\evil2",
            r"C:\Projects\sneaky",
        ] {
            assert!(!trusted(p, &fs), "{p}");
        }
    }

    #[test]
    fn an_alias_that_stays_inside_the_prefix_is_still_refused_because_the_dialog_must_equal_the_filesystem(
    ) {
        let fs = Fake::with(&[r"C:\Projects\real"]).alias(r"C:\Projects\link", r"C:\Projects\real");
        assert!(!trusted(r"C:\Projects\link", &fs));
    }

    #[test]
    fn a_missing_folder_and_a_redirected_prefix_are_refused() {
        let fs = Fake::with(&[]);
        assert!(!trusted(r"C:\Projects\ghost", &fs));
        let moved = Fake::with(&[r"C:\Projects\x"]).alias(PREFIX, r"D:\elsewhere");
        assert!(
            !trusted(r"C:\Projects\x", &moved),
            "control: only the prefix redirect differs from the first test"
        );
    }

    const DIALOG: &str = "\
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────\n\
\n\
 Accessing workspace:\n\
\n\
 C:\\Projects\\configurant\n\
\n\
 Quick safety check: Is this a project you created or one you trust?\n";

    #[test]
    fn the_dialog_folder_is_the_single_unwrapped_row_after_the_header() {
        assert_eq!(
            dialog_folder(DIALOG).as_deref(),
            Some(r"C:\Projects\configurant")
        );
        // A hard-wrapped path continues on the next row: refused.
        let wrapped = DIALOG.replace("configurant\n\n", "configu\nrant\n\n");
        assert_eq!(dialog_folder(&wrapped), None);
        // Two headers (one could be quoted text): refused.
        assert_eq!(dialog_folder(&format!("{DIALOG}{DIALOG}")), None);
        // No header at all.
        assert_eq!(dialog_folder("C:\\Projects\\x\n"), None);
    }

    /// The real filesystem, on Windows: a real directory is trusted, and a real NTFS junction out of
    /// the prefix (no administrator needed to make one) is refused after canonicalisation.
    #[cfg(windows)]
    #[test]
    fn the_real_filesystem_trusts_a_directory_and_refuses_a_junction_out() {
        let root = tempfile::tempdir().unwrap();
        let root = FsResolver
            .canonical(root.path().to_str().unwrap())
            .unwrap_or_else(|| panic!("canonical"));
        let prefix = format!(r"{root}\prefix");
        let outside = format!(r"{root}\outside");
        std::fs::create_dir_all(format!(r"{prefix}\ok")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let junction = format!(r"{prefix}\evil");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J", &junction, &outside])
            .output()
            .unwrap();
        assert!(
            made.status.success(),
            "mklink /J failed: {}",
            String::from_utf8_lossy(&made.stdout)
        );
        let ok = format!(r"{prefix}\ok");
        assert_eq!(
            trusted_folder(&prefix, &ok, &FsResolver).as_deref(),
            Some(ok.as_str())
        );
        assert_eq!(
            trusted_folder(&prefix, &junction, &FsResolver),
            None,
            "the junction leaves the prefix"
        );
        assert_eq!(
            trusted_folder(&prefix, &format!(r"{prefix}\missing"), &FsResolver),
            None
        );
        // Case differences are the same folder on NTFS.
        assert!(trusted_folder(&prefix, &ok.to_uppercase(), &FsResolver).is_some());
    }
}
