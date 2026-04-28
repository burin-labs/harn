//! Filtered directory walker + sensitive-file filter.
//!
//! Depth-first walk that prunes noisy directories before descending and
//! refuses to ingest credential-shaped files. Kept self-contained so the
//! `tools/search` crate can use `ignore` for honoring `.gitignore` while
//! the indexer stays deterministic across systems regardless of the user's
//! ignore configuration.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Maximum file size accepted by the indexer. Larger files are skipped to
/// avoid blowing up the index on minified bundles, generated protobufs, etc.
pub(crate) const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Directory names whose entire subtree should be skipped. Matched by
/// basename only.
const SKIP_DIRS: &[&str] = &[
    // VCS
    ".git",
    ".hg",
    ".svn",
    // Package managers / lockfiles / caches
    "node_modules",
    "bower_components",
    "vendor",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".venv",
    "venv",
    "env",
    ".tox",
    ".gradle",
    ".idea",
    ".vs",
    ".vscode",
    "target",
    "Pods",
    ".dart_tool",
    ".pub-cache",
    // JS/TS build output
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
    ".parcel-cache",
    ".cache",
    // Swift / Apple
    ".build",
    "DerivedData",
    ".swiftpm",
    // Test coverage
    "coverage",
    ".nyc_output",
    // Legacy host cache
    ".burin",
    // Claude Code
    ".claude",
    // Misc
    ".DS_Store",
    ".tmp",
    ".temp",
    "tmp",
    "temp",
];

const INDEXABLE_EXTENSIONS: &[&str] = &[
    "swift", "m", "mm", "h", "c", "cc", "cpp", "hpp", "cxx", "py", "pyi", "ts", "tsx", "js", "jsx",
    "mjs", "cjs", "go", "rs", "java", "kt", "kts", "scala", "rb", "php", "cs", "fs", "fsx", "lua",
    "r", "jl", "dart", "elm", "sh", "bash", "zsh", "fish", "sql", "ex", "exs", "erl", "hrl", "hs",
    "lhs", "zig", "zon", "harn", "sc", "md", "mdx", "rst", "rmd", "json", "yaml", "yml", "toml",
    "xml", "html", "css", "scss",
];

const EXTENSIONLESS_ALLOWED: &[&str] = &["dockerfile", "makefile", "rakefile"];

const ALLOWED_DOTFILES: &[&str] = &[".env.example", ".gitignore", ".dockerignore"];

pub(crate) fn is_indexable_file(path: &Path) -> bool {
    let lower_ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());
    if let Some(ext) = lower_ext.filter(|e| !e.is_empty()) {
        return INDEXABLE_EXTENSIONS.contains(&ext.as_str());
    }
    let base = path
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    EXTENSIONLESS_ALLOWED.contains(&base.as_str())
}

/// Best-effort language tag for the supplied extension. Unknown extensions
/// return the extension itself so downstream tools can route
/// language-specific behaviour.
pub(crate) fn language_for_extension(ext: &str) -> &str {
    match ext {
        "py" | "pyi" => "python",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "rs" => "rust",
        "swift" => "swift",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "rb" => "ruby",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" => "cpp",
        "cs" => "csharp",
        "php" => "php",
        "zig" => "zig",
        "harn" => "harn",
        "scala" => "scala",
        "ex" | "exs" => "elixir",
        "hs" | "lhs" => "haskell",
        "lua" => "lua",
        "r" => "r",
        other => other,
    }
}

/// Public alias for [`is_sensitive`] — the `state` module needs to call
/// the same predicate from outside this file.
pub(crate) fn is_sensitive_path(path: &Path) -> bool {
    is_sensitive(path)
}

/// Returns `true` if the path looks like a credentials/secrets file that
/// must never enter the index.
pub(crate) fn is_sensitive(path: &Path) -> bool {
    let lower = path.to_string_lossy().to_ascii_lowercase();
    let base = Path::new(&lower)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();

    if EXACT_SENSITIVE.contains(&base.as_str()) {
        return true;
    }
    if BASE_PREFIXES.iter().any(|p| base.starts_with(p)) {
        return true;
    }
    if BASE_SUFFIXES.iter().any(|s| base.ends_with(s)) {
        return true;
    }
    if BASE_CONTAINS.iter().any(|s| base.contains(s)) {
        return true;
    }
    let parts: Vec<&str> = lower.split('/').collect();
    parts
        .iter()
        .any(|part| SENSITIVE_DIRS.contains(&part.to_string().as_str()))
}

const EXACT_SENSITIVE: &[&str] = &[
    ".env",
    ".envrc",
    ".netrc",
    ".pgpass",
    "credentials",
    "credentials.json",
    "credentials.yml",
    "credentials.yaml",
    "secrets",
    "secrets.json",
    "secrets.yml",
    "secrets.yaml",
    "service-account.json",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "id_rsa.pub",
    "id_dsa.pub",
    "id_ecdsa.pub",
    "id_ed25519.pub",
    "authorized_keys",
    "known_hosts",
];

const BASE_PREFIXES: &[&str] = &[".env.", ".env_", "credentials.", "secrets."];

const BASE_SUFFIXES: &[&str] = &[
    ".pem",
    ".key",
    ".p12",
    ".pfx",
    ".keystore",
    ".jks",
    ".asc",
    ".gpg",
    ".crt",
    ".cer",
];

const BASE_CONTAINS: &[&str] = &[
    "private_key",
    "privatekey",
    "api_key",
    "apikey",
    "secret_key",
    "secretkey",
];

const SENSITIVE_DIRS: &[&str] = &[".ssh", ".aws", ".gnupg"];

/// Walk `root` recursively, yielding every absolute path that passes the
/// indexer's filters. Walks are depth-first; the order within a directory
/// is sorted lexicographically so two runs over identical inputs return
/// identical lists (helpful for snapshot tests).
pub(crate) fn walk_indexable<F: FnMut(&Path)>(root: &Path, mut on_file: F) {
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    let skip_dirs: HashSet<&str> = SKIP_DIRS.iter().copied().collect();
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        let mut sorted: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        sorted.sort();
        for entry in sorted {
            let basename = entry
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            if should_skip_basename(&basename, &skip_dirs) {
                continue;
            }
            let metadata = match std::fs::symlink_metadata(&entry) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                if skip_dirs.contains(basename.as_str()) {
                    continue;
                }
                stack.push(entry);
            } else if metadata.is_file() {
                if !is_indexable_file(&entry) {
                    continue;
                }
                if metadata.len() > MAX_FILE_BYTES {
                    continue;
                }
                if is_sensitive(&entry) {
                    continue;
                }
                on_file(&entry);
            }
        }
    }
}

fn should_skip_basename(name: &str, skip_dirs: &HashSet<&str>) -> bool {
    if name.is_empty() {
        return true;
    }
    if skip_dirs.contains(name) {
        // Directories are filtered later (we still want to test the
        // directory-vs-file check before pruning), so don't short-circuit
        // here — the caller handles directory skipping after the metadata
        // probe.
        return false;
    }
    if name.starts_with('.') && name.len() > 1 {
        if ALLOWED_DOTFILES.contains(&name) {
            return false;
        }
        // Dot-prefixed dirs that aren't in `skip_dirs` (e.g. `.cargo`,
        // `.config`) are still walked — only dot-prefixed *files* are
        // dropped. We can't check
        // file-vs-dir here without another stat, so approximate by
        // returning false; the caller's metadata probe will let the dir
        // through and skip the dotfile via `is_indexable_file` (no
        // matching extension).
        return false;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn indexable_extensions_cover_common_languages() {
        assert!(is_indexable_file(Path::new("foo.rs")));
        assert!(is_indexable_file(Path::new("foo.swift")));
        assert!(is_indexable_file(Path::new("Foo.SWIFT")));
        assert!(is_indexable_file(Path::new("Dockerfile")));
        assert!(!is_indexable_file(Path::new("foo.bin")));
        assert!(!is_indexable_file(Path::new("README"))); // no extension, not whitelisted
    }

    #[test]
    fn sensitive_filter_rejects_known_shapes() {
        assert!(is_sensitive(Path::new("/repo/.env")));
        assert!(is_sensitive(Path::new("/repo/.env.local")));
        assert!(is_sensitive(Path::new("/repo/secrets.yaml")));
        assert!(is_sensitive(Path::new("/repo/server.pem")));
        assert!(is_sensitive(Path::new("/repo/api_key.txt")));
        assert!(is_sensitive(Path::new("/Users/me/.ssh/id_rsa")));
        assert!(!is_sensitive(Path::new("/repo/src/main.rs")));
    }

    #[test]
    fn walk_skips_pruned_dirs_and_oversize_files() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("node_modules/foo")).unwrap();
        fs::create_dir_all(root.join(".git/objects")).unwrap();
        fs::write(root.join("src/main.rs"), b"fn main() {}").unwrap();
        fs::write(root.join("src/.env"), b"SECRET=x").unwrap();
        fs::write(root.join("node_modules/foo/lib.js"), b"x").unwrap();
        fs::write(root.join(".git/objects/pack"), b"git").unwrap();
        fs::write(
            root.join("oversize.json"),
            vec![b'a'; (MAX_FILE_BYTES + 1) as usize],
        )
        .unwrap();

        let mut found: Vec<String> = Vec::new();
        walk_indexable(root, |p| {
            found.push(
                p.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
                    .replace('\\', "/"),
            );
        });
        found.sort();
        assert_eq!(found, vec!["src/main.rs"]);
    }

    #[test]
    fn languages_are_routed_correctly() {
        assert_eq!(language_for_extension("rs"), "rust");
        assert_eq!(language_for_extension("ts"), "typescript");
        assert_eq!(language_for_extension("py"), "python");
        assert_eq!(language_for_extension("unknown"), "unknown");
    }
}
