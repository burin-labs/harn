//! Lightweight `harn.toml` loader for `harn fmt` and `harn lint`.
//!
//! This module is intentionally separate from `crate::package` (which owns
//! the richer `[check]` + `[dependencies]` manifest model used by
//! `harn check`, `harn install`, etc.). `harn.toml` can carry both sets of
//! keys; this loader focuses on the `[fmt]` and `[lint]` sections and walks
//! up from an input file looking for the nearest manifest.
//!
//! Recognized keys (snake_case, Cargo-style):
//!
//! ```toml
//! [fmt]
//! line_width = 100
//! separator_width = 80
//!
//! [lint]
//! disabled = ["unused-import"]
//! require_file_header = false
//! ```

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

const MANIFEST: &str = "harn.toml";

/// Hard cap on how many parent directories the loader will inspect.
///
/// The walk also stops early at a `.git` boundary (the first directory
/// containing a `.git` child is treated as the project root). The cap
/// exists to defend against pathological paths, symlink loops, and
/// accidental pickup of a stray `harn.toml` high up the filesystem
/// (e.g. a user's home directory or `/tmp`).
const MAX_PARENT_DIRS: usize = 16;

/// Combined `harn.toml` view used by `harn fmt` and `harn lint`.
#[derive(Debug, Default, Clone)]
pub struct HarnConfig {
    pub fmt: FmtConfig,
    pub lint: LintConfig,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct FmtConfig {
    #[serde(default, alias = "line-width")]
    pub line_width: Option<usize>,
    #[serde(default, alias = "separator-width")]
    pub separator_width: Option<usize>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct LintConfig {
    #[serde(default)]
    pub disabled: Option<Vec<String>>,
    /// Opt-in file-header requirement. Accept both snake_case (canonical,
    /// Cargo-style) and kebab-case (rule-name style) so authors who copy
    /// the rule's diagnostic name into their TOML don't silently get
    /// `false`.
    #[serde(default, alias = "require-file-header")]
    pub require_file_header: Option<bool>,
}

/// The parsed shape of the sections this loader cares about.  Kept private;
/// callers get the flattened `HarnConfig` instead.
#[derive(Debug, Default, Deserialize)]
struct RawManifest {
    #[serde(default)]
    fmt: FmtConfig,
    #[serde(default)]
    lint: LintConfig,
}

#[derive(Debug)]
pub enum ConfigError {
    Parse {
        path: PathBuf,
        message: String,
    },
    #[allow(dead_code)]
    Io {
        path: PathBuf,
        error: std::io::Error,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Parse { path, message } => {
                write!(f, "failed to parse {}: {message}", path.display())
            }
            ConfigError::Io { path, error } => {
                write!(f, "failed to read {}: {error}", path.display())
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// Walks up from `start` to find the nearest `harn.toml`. Returns
/// `Ok(HarnConfig::default())` if none is found. Returns `Err` on parse
/// failure so callers can surface the problem rather than silently ignore
/// malformed config.
pub fn load_for_path(start: &Path) -> Result<HarnConfig, ConfigError> {
    // Normalize to an absolute path for robust upward walking. If the path
    // is relative and doesn't exist on disk yet, fall back to using CWD as
    // the base — the walk still terminates at the filesystem root.
    let base = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(start)
    };

    // Walk up from the parent directory of `start` (if `start` itself is a
    // file) or from `start` (if it's a directory).
    let mut cursor: Option<PathBuf> = if base.is_dir() {
        Some(base)
    } else {
        base.parent().map(Path::to_path_buf)
    };

    let mut steps = 0usize;
    while let Some(dir) = cursor {
        if steps >= MAX_PARENT_DIRS {
            break;
        }
        steps += 1;
        let candidate = dir.join(MANIFEST);
        if candidate.is_file() {
            return parse_manifest(&candidate);
        }
        // Stop at project roots — a `.git` directory or file (worktree
        // link) means we've left the user's project and are about to
        // traverse into shared/home/system territory where picking up
        // a stray `harn.toml` would surprise the author.
        if dir.join(".git").exists() {
            break;
        }
        cursor = dir.parent().map(Path::to_path_buf);
    }

    Ok(HarnConfig::default())
}

fn parse_manifest(path: &Path) -> Result<HarnConfig, ConfigError> {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Ok(HarnConfig::default()),
    };
    let raw: RawManifest = toml::from_str(&content).map_err(|e| ConfigError::Parse {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    Ok(HarnConfig {
        fmt: raw.fmt,
        lint: raw.lint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write as _;

    fn write_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        let mut f = File::create(&path).expect("create file");
        f.write_all(content.as_bytes()).expect("write");
        path
    }

    #[test]
    fn no_manifest_yields_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_file(tmp.path(), "main.harn", "pipeline default(t) {}\n");
        let cfg = load_for_path(&harn_file).expect("load");
        assert!(cfg.fmt.line_width.is_none());
        assert!(cfg.fmt.separator_width.is_none());
        assert!(cfg.lint.disabled.is_none());
        assert!(cfg.lint.require_file_header.is_none());
    }

    #[test]
    fn full_config_parses() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(
            tmp.path(),
            "harn.toml",
            r#"
[fmt]
line_width = 120
separator_width = 60

[lint]
disabled = ["unused-import", "missing-harndoc"]
require_file_header = true
"#,
        );
        let harn_file = write_file(tmp.path(), "main.harn", "pipeline default(t) {}\n");
        let cfg = load_for_path(&harn_file).expect("load");
        assert_eq!(cfg.fmt.line_width, Some(120));
        assert_eq!(cfg.fmt.separator_width, Some(60));
        assert_eq!(
            cfg.lint.disabled.as_deref(),
            Some(["unused-import".to_string(), "missing-harndoc".to_string()].as_slice())
        );
        assert_eq!(cfg.lint.require_file_header, Some(true));
    }

    #[test]
    fn partial_config_leaves_other_keys_default() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(
            tmp.path(),
            "harn.toml",
            r#"
[fmt]
line_width = 80
"#,
        );
        let harn_file = write_file(tmp.path(), "main.harn", "pipeline default(t) {}\n");
        let cfg = load_for_path(&harn_file).expect("load");
        assert_eq!(cfg.fmt.line_width, Some(80));
        assert!(cfg.fmt.separator_width.is_none());
        assert!(cfg.lint.disabled.is_none());
    }

    #[test]
    fn malformed_manifest_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(
            tmp.path(),
            "harn.toml",
            "[fmt]\nline_width = \"not-a-number\"\n",
        );
        let harn_file = write_file(tmp.path(), "main.harn", "pipeline default(t) {}\n");
        match load_for_path(&harn_file) {
            Err(ConfigError::Parse { .. }) => {}
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    #[test]
    fn walks_up_two_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(
            root,
            "harn.toml",
            r#"
[fmt]
separator_width = 42
"#,
        );
        let sub = root.join("a").join("b");
        std::fs::create_dir_all(&sub).unwrap();
        let harn_file = write_file(&sub, "main.harn", "pipeline default(t) {}\n");
        let cfg = load_for_path(&harn_file).expect("load");
        assert_eq!(cfg.fmt.separator_width, Some(42));
    }

    #[test]
    fn kebab_case_keys_are_accepted() {
        // Rule and CLI flag names use kebab-case (e.g. `require-file-header`),
        // so users sensibly reach for dashes in their harn.toml too. The loader
        // must accept both spellings.
        let tmp = tempfile::tempdir().unwrap();
        write_file(
            tmp.path(),
            "harn.toml",
            r#"
[fmt]
line-width = 110
separator-width = 72

[lint]
require-file-header = true
"#,
        );
        let harn_file = write_file(tmp.path(), "main.harn", "pipeline default(t) {}\n");
        let cfg = load_for_path(&harn_file).expect("load");
        assert_eq!(cfg.fmt.line_width, Some(110));
        assert_eq!(cfg.fmt.separator_width, Some(72));
        assert_eq!(cfg.lint.require_file_header, Some(true));
    }

    #[test]
    fn walk_stops_at_git_boundary() {
        // An ancestor `harn.toml` sits above a `.git` dir; the loader
        // must NOT pick it up — that manifest lives in a different
        // project (or the user's home) and silently applying its
        // `[fmt]` / `[lint]` settings would surprise authors.
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path();
        write_file(
            outer,
            "harn.toml",
            r#"
[fmt]
line_width = 999
"#,
        );
        let project = outer.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(project.join(".git")).unwrap();
        let inner = project.join("src");
        std::fs::create_dir_all(&inner).unwrap();
        let harn_file = write_file(&inner, "main.harn", "pipeline default(t) {}\n");
        let cfg = load_for_path(&harn_file).expect("load");
        assert!(
            cfg.fmt.line_width.is_none(),
            "must not pick up harn.toml from above the .git boundary: got {:?}",
            cfg.fmt.line_width,
        );
    }

    #[test]
    fn walk_stops_at_max_depth() {
        // Build > MAX_PARENT_DIRS of nested directories with no
        // harn.toml and no .git. The loader should terminate without
        // recursing all the way to the filesystem root.
        let tmp = tempfile::tempdir().unwrap();
        let mut dir = tmp.path().to_path_buf();
        for i in 0..(MAX_PARENT_DIRS + 4) {
            dir = dir.join(format!("lvl{i}"));
        }
        std::fs::create_dir_all(&dir).unwrap();
        let harn_file = write_file(&dir, "main.harn", "pipeline default(t) {}\n");
        // The walk must not panic, must not hang, and must return
        // defaults even though a theoretical `harn.toml` could be found
        // higher up on some systems.
        let cfg = load_for_path(&harn_file).expect("load");
        assert!(cfg.fmt.line_width.is_none());
    }

    #[test]
    fn ignores_unrelated_sections() {
        // [package] and [dependencies] are handled by crate::package; this
        // loader must not choke on their presence.
        let tmp = tempfile::tempdir().unwrap();
        write_file(
            tmp.path(),
            "harn.toml",
            r#"
[package]
name = "demo"
version = "0.1.0"

[dependencies]
foo = { path = "../foo" }

[fmt]
line_width = 77
"#,
        );
        let harn_file = write_file(tmp.path(), "main.harn", "pipeline default(t) {}\n");
        let cfg = load_for_path(&harn_file).expect("load");
        assert_eq!(cfg.fmt.line_width, Some(77));
    }
}
