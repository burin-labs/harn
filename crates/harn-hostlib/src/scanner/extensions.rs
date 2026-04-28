//! Source-extension and excluded-directory tables for deterministic scans.

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

/// File extensions the scanner indexes.
pub const SOURCE_EXTENSIONS: &[&str] = &[
    "swift",
    "ts",
    "tsx",
    "js",
    "jsx",
    "mjs",
    "cjs",
    "py",
    "go",
    "rs",
    "rb",
    "java",
    "kt",
    "cpp",
    "c",
    "h",
    "hpp",
    "cs",
    "php",
    "vue",
    "svelte",
    "html",
    "css",
    "scss",
    "less",
    "sql",
    "graphql",
    "proto",
    "md",
    "json",
    "yaml",
    "yml",
    "toml",
    "xml",
    "sh",
    "bash",
    "zsh",
    "dart",
    "scala",
    "sc",
    "dockerfile",
    "zig",
    "zon",
    "ex",
    "exs",
    "lua",
    "hs",
    "lhs",
    "r",
    "rmd",
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

/// True when [`should_traverse`] holds *and* the file extension is in
/// [`SOURCE_EXTENSIONS`].
pub fn should_include(relative_path: &str) -> bool {
    if !should_traverse(relative_path) {
        return false;
    }
    let ext = file_extension(relative_path);
    SOURCE_EXTENSIONS.contains(&ext.as_str())
}

/// Lowercase extension, no leading dot. Returns `""` for paths without one.
pub fn file_extension(relative_path: &str) -> String {
    let last = relative_path.rsplit('/').next().unwrap_or(relative_path);
    match last.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() => ext.to_ascii_lowercase(),
        _ => String::new(),
    }
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
    fn include_requires_source_extension() {
        assert!(should_include("src/main.rs"));
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
