//! Run the selected developer Git instead of Apple's `/usr/bin/git` shim.
//!
//! `/usr/bin/git` is an `xcrun` shim that looks up the active toolchain and
//! caches the answer in the per-user temp dir. A confined process cannot reach
//! that cache, so every Git call it starts repeats the lookup, about 0.7 s each.
//! Runtime plumbing such as checkpoints and scans runs several Git commands per
//! turn, so the shim alone added seconds before a prompt started.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Installed Git binaries that honor `xcode-select`, in preference order.
pub(super) const INSTALLED: &[&str] = &[
    "/var/db/xcode_select_link/usr/bin/git",
    "/Library/Developer/CommandLineTools/usr/bin/git",
    "/Applications/Xcode.app/Contents/Developer/usr/bin/git",
];

const SHIM: &str = "/usr/bin/git";

/// The Git to run for `program` when a PATH search would reach the shim.
///
/// Only a bare `git` is redirected, and only when `path` resolves it to the
/// shim, so a Git the user put ahead of it on PATH keeps running. A
/// `DEVELOPER_DIR` the child would see selects its toolchain, as the shim does.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) fn redirect(
    program: &OsStr,
    path: Option<&OsStr>,
    developer_dir: Option<&OsStr>,
    installed: &[&str],
) -> Option<PathBuf> {
    if program != "git" || first_on_path(program, path?)? != Path::new(SHIM) {
        return None;
    }
    let selected = developer_dir
        .map(|dir| Path::new(dir).join("usr/bin/git"))
        .filter(|git| git.is_absolute());
    selected
        .into_iter()
        .chain(installed.iter().map(PathBuf::from))
        .find(|git| git.is_file())
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn first_on_path(program: &OsStr, path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(entries: &[&Path]) -> std::ffi::OsString {
        std::env::join_paths(entries).unwrap()
    }

    #[test]
    fn redirects_only_a_path_search_that_reaches_the_shim() {
        let temp = tempfile::tempdir().unwrap();
        let installed = temp.path().join("installed-git");
        std::fs::write(&installed, "installed").unwrap();
        let installed_list = [installed.to_str().unwrap()];
        let user_bin = temp.path().join("user-bin");
        std::fs::create_dir_all(&user_bin).unwrap();
        let shim_dir = Path::new("/usr/bin");
        let reaches_shim = Path::new(SHIM).is_file();

        let via_shim = paths(&[&user_bin, shim_dir]);
        let expected = reaches_shim.then(|| installed.clone());
        assert_eq!(
            redirect(OsStr::new("git"), Some(&via_shim), None, &installed_list),
            expected
        );
        // Other programs and absolute paths are never redirected.
        assert_eq!(
            redirect(
                OsStr::new("python3"),
                Some(&via_shim),
                None,
                &installed_list
            ),
            None
        );
        assert_eq!(
            redirect(OsStr::new(SHIM), Some(&via_shim), None, &installed_list),
            None
        );

        // A Git ahead of the shim on PATH is the user's choice and keeps running.
        std::fs::write(user_bin.join("git"), "user git").unwrap();
        assert_eq!(
            redirect(OsStr::new("git"), Some(&via_shim), None, &installed_list),
            None
        );
    }

    #[test]
    fn developer_dir_selects_the_toolchain_like_the_shim() {
        if !Path::new(SHIM).is_file() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let developer = temp.path().join("Developer");
        std::fs::create_dir_all(developer.join("usr/bin")).unwrap();
        std::fs::write(developer.join("usr/bin/git"), "selected").unwrap();
        let installed = temp.path().join("installed-git");
        std::fs::write(&installed, "installed").unwrap();
        let path = paths(&[Path::new("/usr/bin")]);
        assert_eq!(
            redirect(
                OsStr::new("git"),
                Some(&path),
                Some(developer.as_os_str()),
                &[installed.to_str().unwrap()]
            ),
            Some(developer.join("usr/bin/git"))
        );
        // A developer dir without Git falls back to the installed list.
        assert_eq!(
            redirect(
                OsStr::new("git"),
                Some(&path),
                Some(temp.path().join("missing").as_os_str()),
                &[installed.to_str().unwrap()]
            ),
            Some(installed)
        );
    }
}
