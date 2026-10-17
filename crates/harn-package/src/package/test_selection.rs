use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use super::{should_exclude_package_entry, PackageTestRoot, PackageTestsConfig, PathEntryKind};

/// A deterministic package-owned selection, including errors from partial walks.
#[derive(Debug, Default)]
pub struct PackageTestSelection {
    pub files: Vec<PathBuf>,
    pub problems: Vec<String>,
}

impl PackageTestsConfig {
    pub fn select_files(&self, package_dir: &Path) -> PackageTestSelection {
        let mut selection = PackageTestSelection::default();
        let default_roots = vec![PackageTestRoot::Path("tests".to_string())];
        let roots = self.roots.as_ref().unwrap_or(&default_roots);
        if roots.is_empty() {
            selection.problems.push(
                "[tests].roots must not be empty; omit roots for the conventional tests directory"
                    .into(),
            );
        }
        let base = match package_dir.canonicalize() {
            Ok(base) => base,
            Err(error) => {
                selection
                    .problems
                    .push(format!("cannot read package directory: {error}"));
                return selection;
            }
        };
        let mut files = BTreeSet::new();
        'roots: for root in roots {
            let (root, recursive, directory_only, pattern, exclude) = match root {
                PackageTestRoot::Path(path) => (path, true, false, "*.harn", &[][..]),
                PackageTestRoot::Directory(directory) => (
                    &directory.path,
                    directory.recursive,
                    true,
                    directory.pattern.as_deref().unwrap_or("*.harn"),
                    directory.exclude.as_slice(),
                ),
            };
            let matcher = match filename_glob(pattern) {
                Ok(matcher) => matcher,
                Err(error) => {
                    selection.problems.push(error);
                    continue;
                }
            };
            let excluded = match exclude
                .iter()
                .map(|pattern| filename_glob(pattern))
                .collect::<Result<Vec<_>, _>>()
            {
                Ok(excluded) => excluded,
                Err(error) => {
                    selection.problems.push(error);
                    continue;
                }
            };
            let path = Path::new(root);
            if root.trim().is_empty()
                || root.contains('\\')
                || root.contains(':')
                || !path
                    .components()
                    .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
                || !path.components().any(|c| matches!(c, Component::Normal(_)))
            {
                selection.problems.push(format!("invalid [tests].roots path {root:?}: expected a package-relative path without parent traversal"));
                continue;
            }
            let normalized = path
                .components()
                .filter_map(|component| match component {
                    Component::Normal(part) => Some(part),
                    _ => None,
                })
                .collect::<PathBuf>();
            let mut ancestor = PathBuf::new();
            for component in normalized.components() {
                ancestor.push(component);
                if should_exclude_package_entry(&ancestor, PathEntryKind::Directory) {
                    selection.problems.push(format!(
                        "test root {root:?} is excluded from package inputs"
                    ));
                    continue 'roots;
                }
                if fs::symlink_metadata(base.join(&ancestor))
                    .is_ok_and(|metadata| metadata.file_type().is_symlink())
                {
                    selection
                        .problems
                        .push(format!("test root {root:?} traverses a symlink"));
                    continue 'roots;
                }
            }
            let target = base.join(normalized);
            if directory_only && !target.is_dir() {
                selection
                    .problems
                    .push(format!("test root {root:?} must be a readable directory"));
                continue;
            }
            // Only the omitted conventional root may be absent for a testless package.
            if self.roots.is_none()
                && self.allow_empty
                && fs::symlink_metadata(&target)
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            {
                continue;
            }
            let mut root_files = BTreeSet::new();
            if let Err(error) = collect(
                &base,
                &target,
                recursive,
                &matcher,
                &excluded,
                &mut root_files,
            ) {
                selection.problems.push(error);
            } else if root_files.is_empty() && !(self.roots.is_none() && self.allow_empty) {
                selection.problems.push(format!(
                    "test root {root:?} contains no selected .harn files"
                ));
            }
            files.extend(root_files);
        }
        selection.files = files.into_iter().collect();
        selection
    }
}

fn filename_glob(pattern: &str) -> Result<globset::GlobMatcher, String> {
    match globset::Glob::new(pattern) {
        Ok(glob) if !pattern.is_empty() && !pattern.contains(['/', '\\']) => {
            Ok(glob.compile_matcher())
        }
        _ => Err(format!(
            "invalid test filename pattern {pattern:?}: expected a non-empty basename glob"
        )),
    }
}

fn collect(
    base: &Path,
    path: &Path,
    recursive: bool,
    matcher: &globset::GlobMatcher,
    excluded: &[globset::GlobMatcher],
    files: &mut BTreeSet<PathBuf>,
) -> Result<(), String> {
    let fail = |error: std::io::Error| format!("cannot read test path {}: {error}", path.display());
    let canonical = path.canonicalize().map_err(fail)?;
    if !canonical.starts_with(base) {
        return Err(format!("test path {} escapes the package", path.display()));
    }
    let metadata = fs::symlink_metadata(path).map_err(fail)?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "test path {} is a symlink; select its package-owned target directly",
            path.display()
        ));
    }
    let relative = path.strip_prefix(base).map_err(|error| error.to_string())?;
    let kind = if metadata.is_dir() {
        PathEntryKind::Directory
    } else {
        PathEntryKind::File
    };
    if should_exclude_package_entry(relative, kind) {
        return Err(format!(
            "test path {} is excluded from package inputs",
            relative.display()
        ));
    }
    if metadata.is_dir() {
        let mut entries = fs::read_dir(path)
            .map_err(fail)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(fail)?;
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            let entry_path = entry.path();
            let metadata = entry.file_type().map_err(fail)?;
            if metadata.is_dir() && !recursive {
                continue;
            }
            let kind = if metadata.is_dir() {
                PathEntryKind::Directory
            } else {
                PathEntryKind::File
            };
            if should_exclude_package_entry(entry_path.strip_prefix(base).unwrap(), kind) {
                continue;
            }
            if metadata.is_dir()
                || (metadata.is_symlink() && recursive && entry_path.is_dir())
                || (entry_path.extension().is_some_and(|ext| ext == "harn")
                    && entry_path.file_name().is_some_and(|name| {
                        matcher.is_match(name)
                            && !excluded.iter().any(|pattern| pattern.is_match(name))
                    }))
            {
                collect(base, &entry_path, recursive, matcher, excluded, files)?;
            }
        }
    } else if metadata.is_file() && path.extension().is_some_and(|ext| ext == "harn") {
        if path.file_name().is_some_and(|name| {
            matcher.is_match(name) && !excluded.iter().any(|pattern| pattern.is_match(name))
        }) {
            files.insert(canonical);
        }
    } else {
        return Err(format!(
            "test root {} must be a directory or .harn file",
            path.display()
        ));
    }
    Ok(())
}
