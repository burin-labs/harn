//! Lightweight `harn.toml` loader for `harn fmt`, `harn lint`, and
//! `harn eval prompt --fleet-name <name>`.
//!
//! This module is intentionally separate from `crate::package` (which owns
//! the richer `[check]` + `[dependencies]` manifest model used by
//! `harn check`, `harn install`, etc.). `harn.toml` can carry both sets of
//! keys; this loader focuses on the `[fmt]`, `[lint]`, and `[eval.fleets]`
//! sections and walks up from an input file looking for the nearest
//! manifest.
//!
//! Recognized keys (snake_case, Cargo-style):
//!
//! ```toml
//! [fmt]
//! line_width = 100
//! # By default, section-header separators follow line_width.
//! # Set separator_width to force a fixed width.
//!
//! [lint]
//! disabled = ["unused-import"]
//! require_file_header = false
//! require_docstrings = false
//! complexity_threshold = 25
//! persona_step_allowlist = ["legacy_helper"]
//! template_variant_branch_threshold = 3
//!
//! # Reusable fleets consumed by `harn eval prompt --fleet-name <name>`.
//! [eval.fleets.frontier]
//! models = ["claude-opus-4-7", "gpt-5", "gemini-2.5-pro"]
//!
//! [eval.fleets.local]
//! models = ["ollama:qwen3.5", "ollama:llama4"]
//! ```

use std::collections::BTreeMap;
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

/// Combined `harn.toml` view used by `harn fmt`, `harn lint`, and
/// `harn eval prompt`.
#[derive(Debug, Default, Clone)]
pub struct HarnConfig {
    pub fmt: FmtConfig,
    pub lint: LintConfig,
    pub eval: EvalConfig,
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
    /// Opt-in docstring requirement: when true, the `missing-harndoc`
    /// rule warns on public functions without a `/** */` doc comment.
    /// Off by default — out of the box, `pub fn` needs no docs.
    #[serde(default, alias = "require-docstrings")]
    pub require_docstrings: Option<bool>,
    /// Override the default cyclomatic-complexity warning threshold
    /// (see `harn_lint::DEFAULT_COMPLEXITY_THRESHOLD`). Accept both
    /// snake_case and kebab-case for consistency with the other keys.
    #[serde(default, alias = "complexity-threshold")]
    pub complexity_threshold: Option<usize>,
    /// Non-stdlib functions that may be called directly from `@persona`
    /// bodies without being declared as `@step`.
    #[serde(default, alias = "persona-step-allowlist")]
    pub persona_step_allowlist: Vec<String>,
    /// Threshold for the `template-variant-explosion` rule. Defaults
    /// to [`harn_lint::DEFAULT_TEMPLATE_VARIANT_BRANCH_THRESHOLD`].
    #[serde(default, alias = "template-variant-branch-threshold")]
    pub template_variant_branch_threshold: Option<usize>,
    /// `[lint.severity]` — per-rule severity overrides (#2851), a rule id →
    /// `"error"` / `"warning"` / `"info"`. Applied after disable-filtering, so
    /// a project can promote one rule to an error and demote another.
    #[serde(default)]
    pub severity: std::collections::HashMap<String, String>,
}

/// `[eval]` section of `harn.toml`. Reserves a `[eval.fleets.<name>]`
/// table keyed by fleet name; each entry lists the model selectors
/// (alias or `provider:model`) consumed by
/// `harn eval prompt --fleet-name <name>`.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct EvalConfig {
    #[serde(default)]
    pub fleets: BTreeMap<String, EvalFleet>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct EvalFleet {
    #[serde(default)]
    pub models: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawManifest {
    #[serde(default)]
    fmt: FmtConfig,
    #[serde(default)]
    lint: LintConfig,
    #[serde(default)]
    eval: EvalConfig,
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
    // Normalize to an absolute path so the walk works when `start` is a
    // non-existent relative path.
    let base = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(start)
    };

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
        // Stop at a `.git` boundary so a stray `harn.toml` in a parent
        // project or in `$HOME` is never silently picked up.
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
        eval: raw.eval,
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
        assert!(cfg.lint.require_docstrings.is_none());
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
require_docstrings = true
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
        assert_eq!(cfg.lint.require_docstrings, Some(true));
    }

    #[test]
    fn partial_config_leaves_other_keys_default() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(
            tmp.path(),
            "harn.toml",
            r"
[fmt]
line_width = 80
",
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
            r"
[fmt]
separator_width = 42
",
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
            r"
[fmt]
line-width = 110
separator-width = 72

[lint]
require-file-header = true
require-docstrings = true
",
        );
        let harn_file = write_file(tmp.path(), "main.harn", "pipeline default(t) {}\n");
        let cfg = load_for_path(&harn_file).expect("load");
        assert_eq!(cfg.fmt.line_width, Some(110));
        assert_eq!(cfg.fmt.separator_width, Some(72));
        assert_eq!(cfg.lint.require_file_header, Some(true));
        assert_eq!(cfg.lint.require_docstrings, Some(true));
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
            r"
[fmt]
line_width = 999
",
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
    fn eval_fleets_parse_into_named_lookups() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(
            tmp.path(),
            "harn.toml",
            r#"
[eval.fleets.frontier]
models = ["claude-opus-4-7", "gpt-5", "gemini-2.5-pro"]

[eval.fleets.local]
models = ["ollama:qwen3.5"]
"#,
        );
        let harn_file = write_file(tmp.path(), "main.harn", "pipeline default(t) {}\n");
        let cfg = load_for_path(&harn_file).expect("load");
        assert_eq!(cfg.eval.fleets.len(), 2);
        assert_eq!(
            cfg.eval.fleets.get("frontier").map(|f| f.models.as_slice()),
            Some(
                [
                    "claude-opus-4-7".to_string(),
                    "gpt-5".to_string(),
                    "gemini-2.5-pro".to_string(),
                ]
                .as_slice()
            ),
        );
        assert_eq!(
            cfg.eval.fleets.get("local").map(|f| f.models.as_slice()),
            Some(["ollama:qwen3.5".to_string()].as_slice()),
        );
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
