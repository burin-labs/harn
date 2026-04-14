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
//! auto_insert_separators = false
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

/// Combined `harn.toml` view used by `harn fmt` and `harn lint`.
#[derive(Debug, Default, Clone)]
pub struct HarnConfig {
    pub fmt: FmtConfig,
    pub lint: LintConfig,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct FmtConfig {
    pub line_width: Option<usize>,
    pub separator_width: Option<usize>,
    pub auto_insert_separators: Option<bool>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct LintConfig {
    pub disabled: Option<Vec<String>>,
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

    while let Some(dir) = cursor {
        let candidate = dir.join(MANIFEST);
        if candidate.is_file() {
            return parse_manifest(&candidate);
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
        assert!(cfg.fmt.auto_insert_separators.is_none());
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
auto_insert_separators = true

[lint]
disabled = ["unused-import", "missing-harndoc"]
require_file_header = true
"#,
        );
        let harn_file = write_file(tmp.path(), "main.harn", "pipeline default(t) {}\n");
        let cfg = load_for_path(&harn_file).expect("load");
        assert_eq!(cfg.fmt.line_width, Some(120));
        assert_eq!(cfg.fmt.separator_width, Some(60));
        assert_eq!(cfg.fmt.auto_insert_separators, Some(true));
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
        assert!(cfg.fmt.auto_insert_separators.is_none());
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
