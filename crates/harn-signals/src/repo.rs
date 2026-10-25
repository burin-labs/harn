//! Repository identity for a working directory, read from Git's files
//! directly so a hook that runs on every tool call spawns no process.

use std::path::{Path, PathBuf};

/// `owner/name` of the `origin` remote of the repository containing `cwd`.
pub fn repo_slug(cwd: &Path) -> Option<String> {
    let config = git_common_dir(cwd)?.join("config");
    let text = std::fs::read_to_string(config).ok()?;
    let url = origin_url(&text)?;
    slug_from_url(&url)
}

fn git_common_dir(cwd: &Path) -> Option<PathBuf> {
    for dir in cwd.ancestors() {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() {
            return Some(dot_git);
        }
        if dot_git.is_file() {
            // A linked worktree: `.git` names its private git dir, whose
            // `commondir` names the shared one holding `config`.
            let text = std::fs::read_to_string(&dot_git).ok()?;
            let git_dir = resolve(dir, text.trim().strip_prefix("gitdir:")?.trim());
            return Some(match std::fs::read_to_string(git_dir.join("commondir")) {
                Ok(common) => resolve(&git_dir, common.trim()),
                Err(_) => git_dir,
            });
        }
    }
    None
}

fn resolve(base: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn origin_url(config: &str) -> Option<String> {
    let mut in_origin = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_origin = line == "[remote \"origin\"]";
            continue;
        }
        if in_origin {
            if let Some((key, value)) = line.split_once('=') {
                if key.trim() == "url" {
                    return Some(value.trim().to_string());
                }
            }
        }
    }
    None
}

/// Accepts `https://host/owner/name(.git)`, `git@host:owner/name(.git)`, and
/// `ssh://git@host/owner/name`.
pub fn slug_from_url(url: &str) -> Option<String> {
    let path = if let Some((_, rest)) = url.split_once("://") {
        rest.split_once('/')?.1
    } else {
        url.split_once(':')?.1
    };
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = path.rsplit('/');
    let name = parts.next().filter(|part| !part.is_empty())?;
    let owner = parts.next().filter(|part| !part.is_empty())?;
    Some(format!("{owner}/{name}"))
}
