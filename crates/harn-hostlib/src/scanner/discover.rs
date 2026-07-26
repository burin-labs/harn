//! File discovery — walks a project root and produces deterministic
//! `(relative_path, absolute_path)` tuples.
//!
//! Discovery semantics:
//!
//! 1. Ask the scanner's [`GitCapabilities`](super::GitCapabilities) for
//!    `git ls-files --cached --others --exclude-standard` data (so the file
//!    set matches `git status` perfectly when run inside a checkout).
//! 2. Fall back to a `walkdir`/`ignore` walk when Git data is unavailable.
//!    The fallback honors `.gitignore` and the
//!    [`super::extensions::EXCLUDED_DIRS`] table.
//! 3. Filter to source extensions and de-duplicate.

use harn_vm::ignore_policy::{self, IgnorePolicy};
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

use crate::scanner::extensions::{is_excluded_dir, should_include, should_traverse};
use crate::scanner::GitCapabilities;

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
pub fn discover_files(
    root: &Path,
    opts: DiscoverOptions,
    git: &dyn GitCapabilities,
) -> Vec<DiscoveredFile> {
    let mut files = git_ls_files(root, git).unwrap_or_else(|| walk_files(root, opts));
    files.retain(|entry| should_include(&entry.relative_path));
    files.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    files.dedup_by(|a, b| a.relative_path == b.relative_path);
    files
}

/// Tunable knobs for [`discover_files`].
#[derive(Clone, Copy, Debug, Default)]
pub struct DiscoverOptions {
    /// Include hidden (`.foo`) entries.
    pub include_hidden: bool,
    /// How much of the shared ignore stack applies.
    pub ignore_policy: IgnorePolicy,
}

fn git_ls_files(root: &Path, git: &dyn GitCapabilities) -> Option<Vec<DiscoveredFile>> {
    let paths = git.list_files(root)?;
    let mut entries = Vec::new();
    let mut saw_path = false;
    for path in paths {
        if path.is_empty() {
            continue;
        }
        saw_path = true;
        if !should_traverse(&path) {
            continue;
        }
        entries.push(DiscoveredFile {
            absolute_path: root.join(&path),
            relative_path: path,
        });
    }
    if saw_path {
        Some(entries)
    } else {
        None
    }
}

fn walk_files(root: &Path, opts: DiscoverOptions) -> Vec<DiscoveredFile> {
    let mut walker = WalkBuilder::new(root);
    // The scanner skips exactly what every other Harn walk skips; the extra
    // `EXCLUDED_DIRS` filter below is scanner-specific and deliberately
    // broader (editor and package-manager caches a source scan never wants).
    //
    // A configuration failure means only that the built-in directory layer
    // could not be materialized, and the walk is already correct without it.
    // This function has no error channel, and over-including a few build
    // directories is a visible, recoverable scan; returning an empty file list
    // would read as "this project has no source" and be silently wrong.
    let _ = ignore_policy::configure(&mut walker, root, opts.ignore_policy, opts.include_hidden);
    walker.filter_entry(|entry| {
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

        let files = discover_files(root, DiscoverOptions::default(), &NoGit);
        let names: Vec<_> = files.iter().map(|f| f.relative_path.as_str()).collect();
        assert!(names.contains(&"src/main.rs"));
        assert!(names.contains(&"README.md"));
        assert!(!names.iter().any(|n| n.starts_with("node_modules")));
    }

    #[test]
    fn git_file_list_comes_from_injected_capability() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/tracked.rs"), "fn tracked() {}\n").unwrap();
        fs::write(root.join("src/unlisted.rs"), "fn unlisted() {}\n").unwrap();

        let git = MockGit {
            files: vec!["src/tracked.rs".to_string()],
        };
        let files = discover_files(root, DiscoverOptions::default(), &git);
        let names: Vec<_> = files.iter().map(|f| f.relative_path.as_str()).collect();
        assert_eq!(names, vec!["src/tracked.rs"]);
    }

    struct NoGit;

    impl GitCapabilities for NoGit {
        fn list_files(&self, _root: &Path) -> Option<Vec<String>> {
            None
        }

        fn churn_scores(&self, _root: &Path) -> std::collections::BTreeMap<String, f64> {
            std::collections::BTreeMap::new()
        }
    }

    struct MockGit {
        files: Vec<String>,
    }

    impl GitCapabilities for MockGit {
        fn list_files(&self, _root: &Path) -> Option<Vec<String>> {
            Some(self.files.clone())
        }

        fn churn_scores(&self, _root: &Path) -> std::collections::BTreeMap<String, f64> {
            std::collections::BTreeMap::new()
        }
    }
}
