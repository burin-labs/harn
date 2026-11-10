//! Run the selected developer Git instead of Apple's `/usr/bin/git` shim.
//!
//! `/usr/bin/git` is an `xcrun` shim that looks up the active toolchain and
//! caches the answer in the per-user temp dir. A confined process cannot reach
//! that cache, so every Git call it starts repeats the lookup, about 0.7 s each.
//! Runtime plumbing such as checkpoints and scans runs several Git commands per
//! turn, so the shim alone added seconds before a prompt started.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};

/// Installed Git binaries that honor `xcode-select`, in preference order.
pub(super) const INSTALLED: &[&str] = &[
    "/var/db/xcode_select_link/usr/bin/git",
    "/Library/Developer/CommandLineTools/usr/bin/git",
    "/Applications/Xcode.app/Contents/Developer/usr/bin/git",
];

const SHIM: &str = "/usr/bin/git";

type RedirectKey = (Option<OsString>, Option<OsString>);

/// Answers by PATH and `DEVELOPER_DIR`, so a process resolves each pair once.
static REDIRECTS: LazyLock<Mutex<HashMap<RedirectKey, Option<PathBuf>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// [`redirect`] for a bare `git`, resolved once per PATH and `DEVELOPER_DIR`.
pub(super) fn cached_redirect(
    path: Option<&OsStr>,
    developer_dir: Option<&OsStr>,
) -> Option<PathBuf> {
    let key = (
        path.map(OsStr::to_os_string),
        developer_dir.map(OsStr::to_os_string),
    );
    let mut redirects = REDIRECTS.lock().unwrap_or_else(PoisonError::into_inner);
    redirects
        .entry(key)
        .or_insert_with(|| redirect(OsStr::new("git"), path, developer_dir, INSTALLED))
        .clone()
}

/// The Git to run for `program` when a PATH search would reach the shim.
///
/// Only a bare `git` is redirected, and only when `path` resolves it to the
/// shim, so a Git the user put ahead of it on PATH keeps running. A
/// `DEVELOPER_DIR` the child would see selects its toolchain, as the shim does,
/// but only when it names a developer directory holding a real `usr/bin/git`;
/// otherwise the fixed installed list decides. Nothing else is ever run.
pub(super) fn redirect(
    program: &OsStr,
    path: Option<&OsStr>,
    developer_dir: Option<&OsStr>,
    installed: &[&str],
) -> Option<PathBuf> {
    if program != "git" || first_on_path(program, path?)? != Path::new(SHIM) {
        return None;
    }
    developer_dir.and_then(selected_developer_git).or_else(|| {
        installed
            .iter()
            .map(PathBuf::from)
            .find(|git| is_executable_file(git))
    })
}

/// `DEVELOPER_DIR/usr/bin/git`, when the directory is absolute and the Git in
/// it is an executable regular file rather than a link to somewhere else.
fn selected_developer_git(dir: &OsStr) -> Option<PathBuf> {
    let dir = Path::new(dir);
    if !dir.is_absolute() || !dir.is_dir() {
        return None;
    }
    let git = dir.join("usr/bin/git");
    let meta = std::fs::symlink_metadata(&git).ok()?;
    (meta.file_type().is_file() && is_executable_file(&git)).then_some(git)
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

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

    fn executable(path: &Path) {
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
    }

    #[test]
    fn redirects_only_a_path_search_that_reaches_the_shim() {
        let temp = tempfile::tempdir().unwrap();
        let installed = temp.path().join("installed-git");
        executable(&installed);
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
        executable(&developer.join("usr/bin/git"));
        let installed = temp.path().join("installed-git");
        executable(&installed);
        let path = paths(&[Path::new("/usr/bin")]);
        let installed_list = [installed.to_str().unwrap()];
        let choose = |developer_dir: &Path| {
            redirect(
                OsStr::new("git"),
                Some(&path),
                Some(developer_dir.as_os_str()),
                &installed_list,
            )
        };
        assert_eq!(choose(&developer), Some(developer.join("usr/bin/git")));

        // A developer dir that is missing, relative, or whose Git is a link
        // or not executable is ignored, and the installed list decides.
        assert_eq!(
            choose(&temp.path().join("missing")),
            Some(installed.clone())
        );
        assert_eq!(choose(Path::new("Developer")), Some(installed.clone()));
        let linked = temp.path().join("Linked");
        std::fs::create_dir_all(linked.join("usr/bin")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&installed, linked.join("usr/bin/git")).unwrap();
        assert_eq!(choose(&linked), Some(installed.clone()));
        let plain = temp.path().join("Plain");
        std::fs::create_dir_all(plain.join("usr/bin")).unwrap();
        std::fs::write(plain.join("usr/bin/git"), "not executable").unwrap();
        assert_eq!(choose(&plain), Some(installed.clone()));

        // With no usable Git anywhere, nothing is redirected.
        let missing = temp.path().join("absent-git");
        assert_eq!(
            redirect(
                OsStr::new("git"),
                Some(&path),
                Some(plain.as_os_str()),
                &[missing.to_str().unwrap()]
            ),
            None
        );
    }
}
