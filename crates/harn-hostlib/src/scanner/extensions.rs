//! Source-file and excluded-directory rules for deterministic scans.

use harn_languages::Language;

/// Directory names that the scanner refuses to traverse, regardless of
/// `.gitignore` state.
pub const EXCLUDED_DIRS: &[&str] = &[
    ".git",
    ".build",
    ".burin",
    ".harn",
    ".harn-runs",
    "node_modules",
    ".next",
    "dist",
    "build",
    "__pycache__",
    ".venv",
    "venv",
    ".tox",
    "target",
    ".gradle",
    ".idea",
    ".vscode",
    "Pods",
    "DerivedData",
    ".swiftpm",
    "vendor",
    ".cache",
    ".nuget",
    "coverage",
    ".nyc_output",
    ".turbo",
    ".parcel-cache",
];

/// Returns true when the `name` segment of a path is in [`EXCLUDED_DIRS`].
pub fn is_excluded_dir(name: &str) -> bool {
    EXCLUDED_DIRS.contains(&name)
}

/// True when no segment of `relative_path` is in [`EXCLUDED_DIRS`].
pub fn should_traverse(relative_path: &str) -> bool {
    relative_path
        .split('/')
        .all(|segment| !is_excluded_dir(segment))
}

/// True when [`should_traverse`] holds *and* the language registry
/// recognizes the file by name or extension.
pub fn should_include(relative_path: &str) -> bool {
    should_traverse(relative_path) && file_language(relative_path).is_some()
}

/// The registry language of a posix-style relative path.
pub fn file_language(relative_path: &str) -> Option<Language> {
    Language::from_path(relative_path)
}

/// Last path component (file name) for a posix-style relative path.
pub fn file_name(relative_path: &str) -> &str {
    relative_path.rsplit('/').next().unwrap_or(relative_path)
}

/// Parent directory in posix style. Returns `""` for top-level files.
pub fn parent_dir(relative_path: &str) -> &str {
    match relative_path.rsplit_once('/') {
        Some((parent, _)) => parent,
        None => "",
    }
}

/// Folder key used by [`super::folders`] aggregation: parent directory or
/// `"."` for repo root.
pub fn folder_key(relative_path: &str) -> &str {
    let parent = parent_dir(relative_path);
    if parent.is_empty() {
        "."
    } else {
        parent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excluded_segments_block_traversal() {
        assert!(!should_traverse("node_modules/foo/bar.js"));
        assert!(!should_traverse(".git/HEAD"));
        assert!(should_traverse("src/lib.rs"));
    }

    #[test]
    fn include_requires_a_registry_language() {
        assert!(should_include("src/main.rs"));
        assert!(should_include("app/Dockerfile"));
        assert!(should_include("src/build.gradle.kts"));
        assert!(!should_include("src/main.txt"));
        assert!(!should_include("README"));
    }

    #[test]
    fn parent_and_folder_key() {
        assert_eq!(parent_dir("src/lib.rs"), "src");
        assert_eq!(parent_dir("Cargo.toml"), "");
        assert_eq!(folder_key("Cargo.toml"), ".");
        assert_eq!(folder_key("src/lib.rs"), "src");
    }
}
