//! File discovery — walks a project root and produces deterministic
//! `(relative_path, absolute_path)` tuples.
//!
//! Discovery semantics:
//!
//! 1. Try `git ls-files --cached --others --exclude-standard` (so the file
//!    set matches `git status` perfectly when run inside a checkout).
//! 2. Fall back to a `walkdir`/`ignore` walk that honors `.gitignore` and
//!    the [`super::extensions::EXCLUDED_DIRS`] table.
//! 3. Filter to source extensions and de-duplicate.

use std::path::{Path, PathBuf};
use std::process::Command;

use ignore::WalkBuilder;

use crate::scanner::extensions::{is_excluded_dir, should_include, should_traverse};

/// One discovered file. Paths are stored side-by-side because the scanner
/// reads each file by absolute path but stores everything under
/// `relative_path` in [`super::result::FileRecord`].
#[derive(Clone, Debug)]
pub struct DiscoveredFile {
    /// POSIX-style path relative to the scan root.
    pub relative_path: String,
    /// Absolute path on disk.
    pub absolute_path: PathBuf,
}

/// Run discovery against `root`. Returns deterministic, alphabetically
/// sorted entries.
pub fn discover_files(root: &Path, opts: DiscoverOptions) -> Vec<DiscoveredFile> {
    let mut files = git_ls_files(root).unwrap_or_default();
    if files.is_empty() {
        files = walk_files(root, opts);
    }
    files.retain(|entry| should_include(&entry.relative_path));
    files.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    files.dedup_by(|a, b| a.relative_path == b.relative_path);
    files
}

/// Tunable knobs for [`discover_files`].
#[derive(Clone, Copy, Debug)]
pub struct DiscoverOptions {
    /// Include hidden (`.foo`) entries.
    pub include_hidden: bool,
    /// Honor `.gitignore`/`.git/info/exclude` chains.
    pub respect_gitignore: bool,
}

impl Default for DiscoverOptions {
    fn default() -> Self {
        Self {
            include_hidden: false,
            respect_gitignore: true,
        }
    }
}

fn git_ls_files(root: &Path) -> Option<Vec<DiscoveredFile>> {
    let output = Command::new("git")
        .args([
            "-C",
            root.to_str()?,
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let mut entries = Vec::new();
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        if !should_traverse(line) {
            continue;
        }
        entries.push(DiscoveredFile {
            relative_path: line.to_string(),
            absolute_path: root.join(line),
        });
    }
    if entries.is_empty() {
        None
    } else {
        Some(entries)
    }
}

fn walk_files(root: &Path, opts: DiscoverOptions) -> Vec<DiscoveredFile> {
    let mut walker = WalkBuilder::new(root);
    walker
        .hidden(!opts.include_hidden)
        .ignore(opts.respect_gitignore)
        .git_ignore(opts.respect_gitignore)
        .git_global(opts.respect_gitignore)
        .git_exclude(opts.respect_gitignore)
        .require_git(false)
        .parents(true)
        .filter_entry(|entry| {
            entry
                .file_name()
                .to_str()
                .map(|name| !is_excluded_dir(name))
                .unwrap_or(true)
        });

    let mut entries = Vec::new();
    for result in walker.build() {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let abs = entry.path().to_path_buf();
        let relative = match abs.strip_prefix(root) {
            Ok(p) => p.to_path_buf(),
            Err(_) => continue,
        };
        let relative_str = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if relative_str.is_empty() {
            continue;
        }
        entries.push(DiscoveredFile {
            relative_path: relative_str,
            absolute_path: abs,
        });
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use tempfile::tempdir;

    #[test]
    fn discovers_source_files_and_skips_excluded_dirs() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("node_modules/foo")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("README.md"), "# hi").unwrap();
        fs::write(root.join("node_modules/foo/bar.js"), "x").unwrap();

        let files = discover_files(root, DiscoverOptions::default());
        let names: Vec<_> = files.iter().map(|f| f.relative_path.as_str()).collect();
        assert!(names.contains(&"src/main.rs"));
        assert!(names.contains(&"README.md"));
        assert!(!names.iter().any(|n| n.starts_with("node_modules")));
    }
}
