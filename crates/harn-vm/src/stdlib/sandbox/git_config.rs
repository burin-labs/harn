//! Read-only roots named by the user's Git configuration.
//!
//! Ask Git to evaluate global/system includes and `includeIf` for each workspace
//! instead of implementing another Git config parser. Local repository config
//! cannot expand the process jail: only `--global` and `--system` are queried.
//! The query is host-side, before the child enters its OS sandbox. Values stay
//! in memory and are never printed because Git config can contain credentials.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::paths::normalize_for_policy;
use super::{
    base_workspace_roots, package_manager_config_read_roots_for_home, process_sandbox_presets,
    sandbox_user_home_dir,
};
use crate::orchestration::{CapabilityPolicy, ProcessSandboxPreset};

#[path = "git_config_cache.rs"]
mod cache;

pub(crate) fn process_sandbox_package_manager_config_read_roots(
    policy: &CapabilityPolicy,
) -> Vec<PathBuf> {
    if !process_sandbox_presets(policy).contains(&ProcessSandboxPreset::PackageManagerConfig) {
        return Vec::new();
    }
    let home = sandbox_user_home_dir();
    let mut roots = home
        .as_deref()
        .map(package_manager_config_read_roots_for_home)
        .unwrap_or_default();
    roots.extend(read_roots_for_workspaces(
        &base_workspace_roots(policy),
        home.as_deref(),
    ));
    roots.sort_unstable();
    roots.dedup();
    roots
}

pub(super) fn read_roots_for_workspaces(
    workspaces: &[PathBuf],
    home: Option<&Path>,
) -> Vec<PathBuf> {
    let mut roots = BTreeSet::new();
    let Some(git) = trusted_git_executable(workspaces) else {
        return Vec::new();
    };
    let env: BTreeMap<_, _> = match crate::stdlib::process::session_closed_env_for_command(
        &git.to_string_lossy(),
        std::iter::empty(),
    ) {
        Ok(Some(env)) => env
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect(),
        Ok(None) => std::env::vars_os().collect(),
        Err(_) => return Vec::new(),
    };
    let effective_home = env.get(std::ffi::OsStr::new("HOME")).map(PathBuf::from);
    let home = effective_home
        .as_deref()
        .filter(|home| home.is_absolute())
        .or(home);
    let mut cache = cache::cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for workspace in workspaces {
        roots.extend(cache.roots(workspace, home, &git, &env, || {
            let mut listings = Vec::new();
            let mut complete = true;
            for scope in ["--global", "--system"] {
                // The confined git reads its config under the session's child
                // environment, so the roots are computed under the same one.
                let Ok(mut command) = crate::process_sandbox::session_std_command(&git) else {
                    complete = false;
                    continue;
                };
                command.env_clear().envs(&env);
                let Ok(output) = command
                    .arg("-C")
                    .arg(workspace)
                    .args([
                        "config",
                        scope,
                        "--includes",
                        "--show-origin",
                        "--null",
                        "--get-regexp",
                        ".*",
                    ])
                    .output()
                else {
                    complete = false;
                    continue;
                };
                // Exit 1 means no matching entries, including an absent default
                // config. It is a measured empty listing, not a query failure.
                if output.status.success() || output.status.code() == Some(1) {
                    listings.push(output.stdout);
                } else {
                    complete = false;
                }
            }
            (listings, complete)
        }));
    }
    roots.extend(env_named_config_files(|key| {
        env.get(std::ffi::OsStr::new(key)).cloned()
    }));
    let workspaces: Vec<_> = workspaces
        .iter()
        .map(|workspace| normalize_for_policy(workspace))
        .collect();
    roots
        .into_iter()
        .filter(|root| {
            !workspaces
                .iter()
                .any(|workspace| root.starts_with(workspace))
        })
        .collect()
}

/// Git opens the files these variables name even when they hold no entries,
/// and an unreadable config file is fatal. An empty file never appears in the
/// origin listing, so an empty job-scoped config would otherwise go ungranted.
fn env_named_config_files(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Vec<PathBuf> {
    ["GIT_CONFIG_GLOBAL", "GIT_CONFIG_SYSTEM"]
        .into_iter()
        .filter_map(var)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.is_file())
        .map(|path| normalize_for_policy(&path))
        .collect()
}

fn trusted_git_executable(workspaces: &[PathBuf]) -> Option<PathBuf> {
    // This runs on the host before confinement. Never resolve `git` through
    // the process current directory or a PATH entry inside the workspace.
    // Apple's /usr/bin/git is an xcrun shim. Use an installed system-owned
    // binary directly so host discovery doesn't warm the workspace cache.
    let installed = if cfg!(target_os = "macos") {
        &[
            "/var/db/xcode_select_link/usr/bin/git",
            "/Library/Developer/CommandLineTools/usr/bin/git",
            "/Applications/Xcode.app/Contents/Developer/usr/bin/git",
        ][..]
    } else {
        &[]
    };
    select_trusted_git(
        workspaces,
        installed,
        std::env::var_os("PATH").as_deref(),
        cfg!(target_os = "macos"),
    )
}

fn select_trusted_git(
    workspaces: &[PathBuf],
    installed: &[&str],
    path: Option<&std::ffi::OsStr>,
    macos: bool,
) -> Option<PathBuf> {
    for path in installed {
        if Path::new(path).is_file() {
            return Some(PathBuf::from(*path));
        }
    }
    if macos {
        if let Some(git) = path.and_then(|path| trusted_git_from_path(workspaces, path, true)) {
            return Some(git);
        }
    }
    #[cfg(unix)]
    if Path::new("/usr/bin/git").is_file() {
        return Some(PathBuf::from("/usr/bin/git"));
    }
    path.and_then(|path| trusted_git_from_path(workspaces, path, false))
}

fn trusted_git_from_path(
    workspaces: &[PathBuf],
    path: &std::ffi::OsStr,
    skip_shim: bool,
) -> Option<PathBuf> {
    let filename = if cfg!(windows) { "git.exe" } else { "git" };
    std::env::split_paths(path)
        .filter(|directory| directory.is_absolute())
        .find_map(|directory| {
            let candidate = directory.join(filename);
            if !candidate.is_file() {
                return None;
            }
            let resolved = normalize_for_policy(&candidate);
            if skip_shim && resolved == Path::new("/usr/bin/git") {
                return None;
            }
            (!workspaces
                .iter()
                .any(|workspace| resolved.starts_with(workspace)))
            .then_some(resolved)
        })
}

fn roots_from_config_listing(
    listing: &[u8],
    workspace: &Path,
    home: Option<&Path>,
) -> Vec<PathBuf> {
    let mut roots = BTreeSet::new();
    let mut fields = listing.split(|byte| *byte == 0);
    while let (Some(origin), Some(entry)) = (fields.next(), fields.next()) {
        let (Ok(origin), Ok(entry)) = (str::from_utf8(origin), str::from_utf8(entry)) else {
            continue;
        };
        if let Some(config_file) = origin.strip_prefix("file:") {
            if let Some(path) =
                named_root(config_file, workspace, home).filter(|path| !path.is_dir())
            {
                roots.insert(path);
            }
        }
        let Some((key, value)) = entry.split_once('\n') else {
            continue;
        };
        if key.starts_with("credential.") && key.ends_with(".helper") {
            // A helper may be a shell snippet or a PATH name. Only a directly
            // named executable is a filesystem root. Never grant a credential
            // store's data file just because it appears in helper arguments.
            if let Some(executable) = shell_words::split(value.trim_start_matches('!'))
                .ok()
                .and_then(|words| words.into_iter().next())
                .filter(|word| Path::new(word).is_absolute() || word.starts_with("~/"))
                .and_then(|word| named_root(&word, workspace, home))
                .filter(|path| !path.is_dir())
            {
                roots.insert(executable);
            }
        } else if matches!(
            key,
            "core.hookspath" | "core.excludesfile" | "core.attributesfile"
        ) {
            if let Some(path) = named_root(value, workspace, home) {
                if key == "core.hookspath" || !path.is_dir() {
                    roots.insert(path);
                }
            }
        }
    }
    roots.into_iter().collect()
}

fn named_root(value: &str, workspace: &Path, home: Option<&Path>) -> Option<PathBuf> {
    if value.is_empty() {
        return None;
    }
    let path = if value == "~" {
        home?.to_path_buf()
    } else if let Some(rest) = value.strip_prefix("~/") {
        home?.join(rest)
    } else if Path::new(value).is_absolute() {
        PathBuf::from(value)
    } else {
        workspace.join(value)
    };
    let path = normalize_for_policy(&path);
    (path.is_absolute() && path.parent().is_some_and(|parent| parent != path)).then_some(path)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn macos_git_selection_prefers_installed_path_git_over_the_shim() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let installed = temp.path().join("installed");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(workspace.join("git"), "workspace executable").unwrap();
        std::fs::write(installed.join("git"), "installed executable").unwrap();
        let path = std::env::join_paths([Path::new("/usr/bin"), &workspace, &installed]).unwrap();
        assert_eq!(
            select_trusted_git(&[normalize_for_policy(&workspace)], &[], Some(&path), true),
            Some(normalize_for_policy(&installed.join("git")))
        );
        let only_workspace = std::env::join_paths([Path::new("/usr/bin"), &workspace]).unwrap();
        assert_eq!(
            select_trusted_git(
                &[normalize_for_policy(&workspace)],
                &[],
                Some(&only_workspace),
                true
            ),
            Some(PathBuf::from("/usr/bin/git"))
        );
    }

    #[test]
    fn config_listing_grants_included_files_and_named_paths_only() {
        let workspace = Path::new("/tmp/harn-git-workspace");
        let home = Path::new("/tmp/harn-git-home");
        let listing = b"file:/tmp/harn-git-home/.gitconfig\0core.hookspath\n/opt/git-hooks\0\
file:/opt/git-config/included\0core.excludesfile\n~/ignore\0\
file:/opt/git-config/included\0core.attributesfile\n../attributes\0\
file:/opt/git-config/included\0core.excludesfile\n/tmp\0\
file:/opt/git-config/included\0credential.helper\n/opt/git-helpers/auth --flag\0\
file:/tmp/harn-git-home/.gitconfig\0user.name\nSomeone\0";
        let roots = roots_from_config_listing(listing, workspace, Some(home));
        for path in [
            "/tmp/harn-git-home/.gitconfig",
            "/opt/git-config/included",
            "/opt/git-hooks",
            "/tmp/harn-git-home/ignore",
            "/opt/git-helpers/auth",
            "/tmp/attributes",
        ] {
            assert!(
                roots.contains(&normalize_for_policy(Path::new(path))),
                "missing {path}"
            );
        }
        assert!(!roots.contains(&normalize_for_policy(Path::new(
            "/tmp/harn-git-home/Someone"
        ))));
        assert!(!roots.contains(&normalize_for_policy(Path::new("/tmp"))));
        let without_home = roots_from_config_listing(listing, workspace, None);
        assert!(without_home.contains(&normalize_for_policy(Path::new("/opt/git-hooks"))));
        assert!(!without_home.contains(&normalize_for_policy(Path::new(
            "/tmp/harn-git-home/ignore"
        ))));
    }

    #[test]
    fn an_empty_env_named_config_file_is_granted() {
        let temp = tempfile::tempdir().unwrap();
        let empty = temp.path().join("job-gitconfig");
        std::fs::write(&empty, "").unwrap();
        let missing = temp.path().join("missing-gitconfig");
        let roots = env_named_config_files(|key| match key {
            "GIT_CONFIG_GLOBAL" => Some(empty.clone().into_os_string()),
            "GIT_CONFIG_SYSTEM" => Some(missing.clone().into_os_string()),
            _ => None,
        });
        assert_eq!(roots, vec![normalize_for_policy(&empty)]);
        assert!(env_named_config_files(|_| Some("relative/gitconfig".into())).is_empty());
    }
}
