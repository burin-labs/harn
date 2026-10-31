//! Workspace scan and the runtime-owned paths it must never attribute.
//!
//! In a git worktree the scan asks git which files differ from the index and
//! stats only those, so its cost follows `git status`, not the size of the
//! tree. Outside git it walks every non-ignored file.

use super::{git, git_command, stat_entry, to_agent_path, GitError, Scan, StatIndex};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

/// What git reported beyond the stat index: a path git does not list matches
/// HEAD, is ignored, or does not exist, so a scan diff resolves it on disk.
#[derive(Default)]
pub(super) struct GitFacts {
    /// HEAD when scanned; `None` before the first commit.
    pub(super) head: Option<String>,
    /// Files git tracks in HEAD or the index that are missing from disk.
    pub(super) absent: BTreeSet<String>,
    /// Listed files that HEAD does not have: untracked or newly added.
    pub(super) added: BTreeSet<String>,
    /// Listed paths that exist but are not regular files (symlinks).
    pub(super) irregular: BTreeSet<String>,
    /// Paths that differ between the previous scan's HEAD and this one's, so a
    /// checkout or reset that rewrote clean files is still seen.
    pub(super) moved: BTreeSet<String>,
}

/// How a path changed between two scans of the same kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ScanChange {
    Created,
    Modified,
    Deleted,
    /// Gone from the scan but still on disk, so its new state is unknown.
    Unseen,
}

pub(super) fn excluded_paths(root: &Path) -> Vec<PathBuf> {
    crate::workspace_runtime_paths::owned_paths(root)
}

pub(super) fn is_excluded(root: &Path, path: &Path, excluded: &[PathBuf]) -> bool {
    if excluded.iter().any(|entry| path.starts_with(entry)) {
        return true;
    }
    path.ancestors()
        .take_while(|ancestor| ancestor.starts_with(root))
        .filter(|ancestor| ancestor.file_name().is_some_and(|name| name == ".harn"))
        .filter_map(Path::parent)
        .filter(|parent| *parent != root)
        .any(|parent| {
            crate::workspace_runtime_paths::owned_paths(parent)
                .iter()
                .any(|entry| path.starts_with(entry))
        })
}

/// Scan a workspace: through git when `root` is in a worktree, else by walking.
pub(super) fn scan_tree(root: &Path, read_parent_ignores: bool) -> Scan {
    git_scan(root).unwrap_or_else(|_| walk_scan(root, read_parent_ignores))
}

/// Scan again the same way `before` was scanned, so the two can be diffed.
pub(super) fn rescan(before: &Scan, root: &Path, read_parent_ignores: bool) -> Scan {
    let Some(was) = &before.git else {
        return walk_scan(root, read_parent_ignores);
    };
    let started = Instant::now();
    let unknown = |started: Instant| Scan {
        index: StatIndex::new(),
        errors: 1,
        error_paths: Vec::new(),
        unlocated_error: true,
        elapsed_ms: started.elapsed().as_millis() as u64,
        git: Some(GitFacts::default()),
    };
    let Ok(mut after) = git_scan(root) else {
        return unknown(started);
    };
    let now = after.git.as_mut().expect("a git scan records git facts");
    if was.head != now.head {
        let (Some(from), Some(to)) = (&was.head, &now.head) else {
            return unknown(started);
        };
        let Ok(listed) = git(
            root,
            &[
                "diff",
                "--name-only",
                "-z",
                "--no-renames",
                "--relative",
                from,
                to,
            ],
        ) else {
            return unknown(started);
        };
        now.moved = nul_paths(&listed).collect();
    }
    after
}

/// Stat what `git status` lists: files that differ from HEAD in the index or
/// on disk, and untracked files that are not ignored. Clean tracked files are
/// not read, because git's index already vouches for them.
pub(super) fn git_scan(root: &Path) -> Result<Scan, GitError> {
    let started = Instant::now();
    let prefix = git(root, &["rev-parse", "--show-prefix"])?;
    let prefix = String::from_utf8_lossy(&prefix)
        .trim_end_matches('\n')
        .to_string();
    let (listed, unseen) = git_status(
        root,
        &[
            "status",
            "--porcelain=v2",
            "-z",
            "--branch",
            "--no-renames",
            "--untracked-files=all",
            "--ignore-submodules=all",
            "--",
            ".",
        ],
    )?;
    let excluded = excluded_paths(root);
    let mut index = StatIndex::new();
    let mut facts = GitFacts::default();
    let mut error_paths = Vec::new();
    let mut unlocated_error = false;
    for path in unseen {
        match path
            .as_deref()
            .and_then(|path| path.strip_prefix(prefix.as_str()))
        {
            Some(path) => error_paths.push(path.trim_end_matches('/').to_string()),
            None => unlocated_error = true,
        }
    }
    for record in listed.split(|byte| *byte == 0) {
        let record = String::from_utf8_lossy(record);
        let (path, added, tracked) = match record.split_once(' ') {
            Some(("#", header)) => {
                if let Some(oid) = header.strip_prefix("branch.oid ") {
                    facts.head = (oid != "(initial)").then(|| oid.to_string());
                }
                continue;
            }
            Some(("?", path)) => (path, true, false),
            Some(("1", rest)) => match rest.splitn(8, ' ').collect::<Vec<_>>()[..] {
                [xy, _, _, _, _, _, _, path] => (path, xy.starts_with('A'), true),
                _ => continue,
            },
            Some(("u", rest)) => match rest.splitn(10, ' ').collect::<Vec<_>>()[..] {
                [.., path] => (path, false, true),
                _ => continue,
            },
            _ => continue,
        };
        // Porcelain paths are relative to the repository, not to `root`.
        let Some(path) = path.strip_prefix(prefix.as_str()) else {
            continue;
        };
        // A trailing slash is a nested repository, which this scan never enters.
        if path.is_empty() || path.ends_with('/') {
            continue;
        }
        let full = root.join(path);
        if is_excluded(root, &full, &excluded) {
            continue;
        }
        let path = path.to_string();
        if added {
            facts.added.insert(path.clone());
        }
        match fs::symlink_metadata(&full) {
            Ok(meta) if meta.is_file() => {
                index.insert(path, stat_entry(&meta));
            }
            Ok(_) => {
                facts.irregular.insert(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if tracked {
                    facts.absent.insert(path);
                }
            }
            Err(_) => error_paths.push(path),
        }
    }
    Ok(Scan {
        index,
        errors: error_paths.len() + usize::from(unlocated_error),
        error_paths,
        unlocated_error,
        elapsed_ms: started.elapsed().as_millis() as u64,
        git: Some(facts),
    })
}

/// Run `git status`, returning its listing and every path it could not read.
/// Git skips a directory it cannot open, or a file it cannot stat, with a
/// message on stderr and still exits 0, so silence there is what proves the
/// listing complete. `None` marks an error git did not tie to a path.
fn git_status(root: &Path, args: &[&str]) -> Result<(Vec<u8>, Vec<Option<String>>), GitError> {
    let output = git_command(root)?
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| GitError::Missing(format!("cannot run git: {e}")))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(GitError::Failed(stderr.trim().to_string()));
    }
    let unseen = stderr
        .lines()
        .filter_map(|line| {
            if let Some(rest) = line.strip_prefix("warning: could not open directory '") {
                return Some(rest.split_once("': ").map(|(path, _)| path.to_string()));
            }
            if line.starts_with("error:") || line.starts_with("fatal:") {
                return Some(None);
            }
            if line.starts_with("warning:") || line.starts_with("hint:") || line.is_empty() {
                return None;
            }
            // `<path>: <reason>` is git reporting a path it could not stat.
            Some(line.rsplit_once(": ").map(|(path, _)| path.to_string()))
        })
        .collect();
    Ok((output.stdout, unseen))
}

fn nul_paths(listed: &[u8]) -> impl Iterator<Item = String> + '_ {
    listed
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
}

/// Stat non-ignored regular files, excluding nested repositories and runtime state.
fn walk_scan(root: &Path, read_parent_ignores: bool) -> Scan {
    scan_tree_with_exclusions(root, read_parent_ignores, excluded_paths(root))
}

pub(super) fn scan_tree_with_exclusions(
    root: &Path,
    read_parent_ignores: bool,
    excluded: Vec<PathBuf>,
) -> Scan {
    let started = Instant::now();
    let walk_root = root.to_path_buf();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .ignore(false)
        .require_git(false)
        .parents(read_parent_ignores)
        .follow_links(false)
        .filter_entry(move |entry| {
            if entry.file_name() == ".git" || is_excluded(&walk_root, entry.path(), &excluded) {
                return false;
            }
            let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
            !(is_dir && entry.path() != walk_root && entry.path().join(".git").exists())
        })
        .build();
    let mut index = StatIndex::new();
    let mut errors = 0;
    let mut error_paths = Vec::new();
    let mut unlocated_error = false;
    let mut note_error = |path: Option<&Path>| {
        errors += 1;
        match path.and_then(|p| p.strip_prefix(root).ok()) {
            Some(rel) => error_paths.push(to_agent_path(rel)),
            None => unlocated_error = true,
        }
    };
    for result in walker {
        let entry = match result {
            Ok(entry) => entry,
            Err(error) => {
                note_error(error_path(&error).as_deref());
                continue;
            }
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            note_error(Some(entry.path()));
            continue;
        };
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        index.insert(to_agent_path(rel), stat_entry(&meta));
    }
    Scan {
        index,
        errors,
        error_paths,
        unlocated_error,
        elapsed_ms: started.elapsed().as_millis() as u64,
        git: None,
    }
}

/// Every path whose state differs between two scans made the same way, sorted.
pub(super) fn diff(root: &Path, before: &Scan, after: &Scan) -> Vec<(String, ScanChange)> {
    let mut changes = Vec::new();
    match (&before.git, &after.git) {
        (Some(was), Some(now)) => {
            let paths: BTreeSet<&String> = before
                .index
                .keys()
                .chain(&was.absent)
                .chain(after.index.keys())
                .chain(&now.absent)
                .chain(&now.moved)
                .collect();
            for path in paths {
                let then = listed_state(before, was, path);
                let change = match (then, listed_state(after, now, path)) {
                    (Listed::Present(a), Listed::Present(b)) if a == b => None,
                    (Listed::Absent, Listed::Absent) => None,
                    (_, Listed::Absent) => Some(ScanChange::Deleted),
                    (Listed::Absent, Listed::Present(_)) => Some(ScanChange::Created),
                    (Listed::Present(_), Listed::Present(_)) => Some(ScanChange::Modified),
                    (Listed::Unlisted, Listed::Present(_)) if now.added.contains(path) => {
                        Some(ScanChange::Created)
                    }
                    (Listed::Unlisted, Listed::Present(_)) => Some(ScanChange::Modified),
                    // Unlisted now: committed, back to HEAD, newly ignored, or gone.
                    (then, Listed::Unlisted) => match fs::symlink_metadata(root.join(path)) {
                        Ok(meta) if meta.is_file() => match then {
                            Listed::Present(prior) if *prior == stat_entry(&meta) => None,
                            Listed::Absent => Some(ScanChange::Created),
                            _ => Some(ScanChange::Modified),
                        },
                        Ok(_) => Some(ScanChange::Unseen),
                        Err(_) if then == Listed::Absent => None,
                        Err(_) => Some(ScanChange::Deleted),
                    },
                };
                if let Some(change) = change {
                    changes.push((path.clone(), change));
                }
            }
        }
        _ => {
            for (path, entry) in &after.index {
                match before.index.get(path) {
                    None => changes.push((path.clone(), ScanChange::Created)),
                    Some(prior) if prior != entry => {
                        changes.push((path.clone(), ScanChange::Modified));
                    }
                    Some(_) => {}
                }
            }
            for path in before.index.keys() {
                if after.index.contains_key(path) {
                    continue;
                }
                // Gone from the scan but still on disk: the scan stopped seeing
                // it (a .gitignore edit, say), which is not a deletion.
                let change = if root.join(path).symlink_metadata().is_ok() {
                    ScanChange::Unseen
                } else {
                    ScanChange::Deleted
                };
                changes.push((path.clone(), change));
            }
            changes.sort_by(|a, b| a.0.cmp(&b.0));
        }
    }
    changes
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Listed<'a> {
    Present(&'a super::StatEntry),
    Absent,
    /// Not listed by git: clean tracked content, ignored, or nonexistent.
    Unlisted,
}

fn listed_state<'a>(scan: &'a Scan, facts: &GitFacts, path: &str) -> Listed<'a> {
    if let Some(entry) = scan.index.get(path) {
        Listed::Present(entry)
    } else if facts.absent.contains(path) {
        Listed::Absent
    } else {
        Listed::Unlisted
    }
}

fn error_path(error: &ignore::Error) -> Option<PathBuf> {
    match error {
        ignore::Error::WithPath { path, .. } => Some(path.clone()),
        ignore::Error::WithDepth { err, .. } | ignore::Error::WithLineNumber { err, .. } => {
            error_path(err)
        }
        _ => None,
    }
}
