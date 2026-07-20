//! Typed project configuration shared by Harn's CLI and language tooling.
//!
//! `harn.toml` can carry many sections. This loader exposes the generic
//! `[fmt]`, `[lint]`, and `[eval.fleets]` policy used by every frontend and
//! walks up from an input file looking for the nearest manifest.
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
//! require_public_api_types = false
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

/// Generic `harn.toml` view shared by the CLI, LSP, and future frontends.
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
    /// Require explicit parameter and return annotations on every public
    /// function and pipeline.
    #[serde(default, alias = "require-public-api-types")]
    pub require_public_api_types: Option<bool>,
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
    /// `[lint.severity]` — typed per-rule severity overrides (#2851). Parsed
    /// here so every frontend observes the same normalized policy.
    #[serde(default)]
    pub severity: std::collections::HashMap<String, LintSeverity>,
}

/// Canonical severity used by project lint configuration and lint diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LintSeverity {
    Info,
    Warning,
    Error,
}

impl<'de> Deserialize<'de> for LintSeverity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.to_ascii_lowercase().as_str() {
            "info" => Ok(Self::Info),
            "warning" | "warn" => Ok(Self::Warning),
            "error" => Ok(Self::Error),
            other => Err(serde::de::Error::custom(format!(
                "unknown lint severity `{other}`; expected `info`, `warning`, or `error`"
            ))),
        }
    }
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

/// Walks up from `start` to find the nearest `harn.toml` via the shared
/// [`manifest_walk`](crate::manifest_walk) walk. Returns
/// `Ok(HarnConfig::default())` if none is found. Returns `Err` on parse
/// failure so callers can surface the problem rather than silently ignore
/// malformed config.
pub fn load_for_path(start: &Path) -> Result<HarnConfig, ConfigError> {
    match crate::manifest_walk::find_nearest_manifest(start) {
        Some(found) => parse_manifest(&found.path),
        None => Ok(HarnConfig::default()),
    }
}

fn parse_manifest(path: &Path) -> Result<HarnConfig, ConfigError> {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        // The manifest existed at `is_file()` time; if it vanished in the
        // race window, fall back to defaults. Any other I/O error (permission
        // denied, bad symlink) is surfaced so a misconfigured manifest never
        // silently degrades to default config.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(HarnConfig::default());
        }
        Err(error) => {
            return Err(ConfigError::Io {
                path: path.to_path_buf(),
                error,
            });
        }
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
        assert!(cfg.lint.require_public_api_types.is_none());
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
require_public_api_types = true

[lint.severity]
missing-public-api-type = "ERROR"
unused-import = "warn"
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
        assert_eq!(cfg.lint.require_public_api_types, Some(true));
        assert_eq!(
            cfg.lint.severity,
            std::collections::HashMap::from([
                ("missing-public-api-type".to_string(), LintSeverity::Error,),
                ("unused-import".to_string(), LintSeverity::Warning),
            ])
        );
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
    fn unknown_lint_severity_is_a_config_error() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(
            tmp.path(),
            "harn.toml",
            "[lint.severity]\nmissing-public-api-type = \"urgent\"\n",
        );
        let harn_file = write_file(tmp.path(), "main.harn", "pipeline default(t) {}\n");
        let error = load_for_path(&harn_file).expect_err("unknown severity must fail closed");
        let ConfigError::Parse { path, message } = error else {
            panic!("expected a typed parse error, got {error:?}");
        };
        assert_eq!(path, tmp.path().join("harn.toml"));
        assert!(
            message
                .contains("unknown lint severity `urgent`; expected `info`, `warning`, or `error`"),
            "serde/toml location prose may vary, but the owned reason must survive: {message}"
        );
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
require-public-api-types = true
",
        );
        let harn_file = write_file(tmp.path(), "main.harn", "pipeline default(t) {}\n");
        let cfg = load_for_path(&harn_file).expect("load");
        assert_eq!(cfg.fmt.line_width, Some(110));
        assert_eq!(cfg.fmt.separator_width, Some(72));
        assert_eq!(cfg.lint.require_file_header, Some(true));
        assert_eq!(cfg.lint.require_docstrings, Some(true));
        assert_eq!(cfg.lint.require_public_api_types, Some(true));
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
        for i in 0..(crate::manifest_walk::MAX_PARENT_DIRS + 4) {
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
