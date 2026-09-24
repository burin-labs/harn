//! Read-only roots named by the user's Git configuration.
//!
//! Ask Git to evaluate global/system includes and `includeIf` for each workspace
//! instead of implementing another Git config parser. Local repository config
//! cannot expand the process jail: only `--global` and `--system` are queried.
//! The query is host-side, before the child enters its OS sandbox. Values stay
//! in memory and are never printed because Git config can contain credentials.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::paths::normalize_for_policy;
use super::{
    base_workspace_roots, package_manager_config_read_roots_for_home, process_sandbox_presets,
    sandbox_user_home_dir,
};
use crate::orchestration::{CapabilityPolicy, ProcessSandboxPreset};

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
    for workspace in workspaces {
        for scope in ["--global", "--system"] {
            let Ok(output) = Command::new(&git)
                .arg("-C")
                .arg(workspace)
                .args([
                    "config",
                    scope,
                    "--includes",
                    "--show-origin",
                    "--null",
                    "--list",
                ])
                .output()
            else {
                continue;
            };
            if output.status.success() {
                roots.extend(roots_from_config_listing(&output.stdout, workspace, home));
            }
        }
    }
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

fn trusted_git_executable(workspaces: &[PathBuf]) -> Option<PathBuf> {
    // This runs on the host before confinement. Never resolve `git` through
    // the process current directory or a PATH entry inside the workspace.
    #[cfg(unix)]
    if Path::new("/usr/bin/git").is_file() {
        return Some(PathBuf::from("/usr/bin/git"));
    }
    let filename = if cfg!(windows) { "git.exe" } else { "git" };
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|directory| directory.is_absolute())
        .find_map(|directory| {
            let candidate = directory.join(filename);
            if !candidate.is_file() {
                return None;
            }
            let resolved = normalize_for_policy(&candidate);
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
}
