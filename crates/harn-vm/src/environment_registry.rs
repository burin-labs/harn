//! Authoritative registry and startup validation for Harn-owned environment
//! variables.
//!
//! Readers remain at their owning boundaries, where values have the context
//! needed for full validation. This module owns names, coarse value shapes,
//! sensitivity metadata, extension policy, and unknown-name diagnostics. The
//! registry drift test keeps production readers from creating an unregistered
//! parallel namespace.

use std::ffi::{OsStr, OsString};
use std::fmt;

const REGISTERED_NAMES: &str = include_str!("environment_registry_names.txt");
const UNKNOWN_CODE: &str = "HARN-ENV-001";
const INVALID_VALUE_CODE: &str = "HARN-ENV-002";

/// The subsystem that owns an environment variable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvironmentConsumer {
    Runtime,
    Cli,
    BuildTooling,
    TestHarness,
    EmbedderExtension,
}

/// The shape enforced at startup, before the owning reader sees the value.
///
/// `OwnerValidated` means the value needs domain context and remains validated
/// by its consumer. The registry still owns and checks the name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvironmentValueShape {
    OwnerValidated,
    Boolean,
    UnsignedInteger,
    NonNegativeNumber,
    UnitInterval,
}

/// Whether a value may contain credential material.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvironmentSensitivity {
    Public,
    Credential,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentVariableSpec {
    pub name: String,
    pub consumer: EnvironmentConsumer,
    pub value_shape: EnvironmentValueShape,
    pub sensitivity: EnvironmentSensitivity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnvironmentDiagnosticKind {
    UnknownName { suggestion: Option<String> },
    InvalidValue { expected: EnvironmentValueShape },
}

/// A key-only diagnostic. Values are intentionally absent from the type, so
/// rendering cannot accidentally disclose credential material.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentDiagnostic {
    pub code: &'static str,
    pub key: String,
    pub kind: EnvironmentDiagnosticKind,
}

impl fmt::Display for EnvironmentDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            EnvironmentDiagnosticKind::UnknownName { suggestion } => {
                write!(
                    formatter,
                    "{}: unknown Harn environment variable `{}`",
                    self.code, self.key
                )?;
                if let Some(suggestion) = suggestion {
                    write!(formatter, "; did you mean `{suggestion}`?")?;
                }
                Ok(())
            }
            EnvironmentDiagnosticKind::InvalidValue { expected } => write!(
                formatter,
                "{}: environment variable `{}` must be {}",
                self.code,
                self.key,
                expected.description()
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentValidationError {
    diagnostics: Vec<EnvironmentDiagnostic>,
}

impl EnvironmentValidationError {
    pub fn diagnostics(&self) -> &[EnvironmentDiagnostic] {
        &self.diagnostics
    }
}

impl fmt::Display for EnvironmentValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, diagnostic) in self.diagnostics.iter().enumerate() {
            if index > 0 {
                formatter.write_str("\n")?;
            }
            diagnostic.fmt(formatter)?;
        }
        Ok(())
    }
}

impl std::error::Error for EnvironmentValidationError {}

impl EnvironmentValueShape {
    fn description(self) -> &'static str {
        match self {
            Self::OwnerValidated => "valid for its owning subsystem",
            Self::Boolean => "a boolean (`true`, `false`, `yes`, `no`, `on`, `off`, `1`, or `0`)",
            Self::UnsignedInteger => "an unsigned integer",
            Self::NonNegativeNumber => "a non-negative number",
            Self::UnitInterval => "a number between 0 and 1",
        }
    }

    fn accepts(self, value: &OsStr) -> bool {
        let Some(value) = value.to_str() else {
            return matches!(self, Self::OwnerValidated);
        };
        match self {
            Self::OwnerValidated => true,
            Self::Boolean => matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "0" | "true" | "false" | "yes" | "no" | "on" | "off"
            ),
            Self::UnsignedInteger => value.trim().parse::<u64>().is_ok(),
            Self::NonNegativeNumber => value
                .trim()
                .parse::<f64>()
                .is_ok_and(|parsed| parsed.is_finite() && parsed >= 0.0),
            Self::UnitInterval => value
                .trim()
                .parse::<f64>()
                .is_ok_and(|parsed| parsed.is_finite() && (0.0..=1.0).contains(&parsed)),
        }
    }
}

/// Look up registry metadata for a Harn-owned key.
pub fn variable_spec(name: &str) -> Option<EnvironmentVariableSpec> {
    if registered_names().binary_search(&name).is_ok() {
        return Some(spec_for_registered_name(name));
    }
    if is_extension_name(name) {
        return Some(EnvironmentVariableSpec {
            name: name.to_string(),
            consumer: EnvironmentConsumer::EmbedderExtension,
            value_shape: EnvironmentValueShape::OwnerValidated,
            sensitivity: sensitivity_for(name),
        });
    }
    if is_structured_runtime_name(name) {
        return Some(spec_for_registered_name(name));
    }
    None
}

/// Validate the live process environment at the CLI/embedded-runtime startup
/// boundary.
pub fn validate_startup_environment() -> Result<(), EnvironmentValidationError> {
    validate_environment(std::env::vars_os())
}

/// Validate a supplied environment snapshot. This is the deterministic core
/// used by hosts and tests; values never enter diagnostics.
pub fn validate_environment<I, K, V>(vars: I) -> Result<(), EnvironmentValidationError>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<OsString>,
    V: Into<OsString>,
{
    let mut diagnostics = Vec::new();
    for (key, value) in vars {
        let key = key.into();
        let Some(key) = key.to_str() else {
            continue;
        };
        if !key.starts_with("HARN_") {
            continue;
        }
        let Some(spec) = variable_spec(key) else {
            diagnostics.push(EnvironmentDiagnostic {
                code: UNKNOWN_CODE,
                key: key.to_string(),
                kind: EnvironmentDiagnosticKind::UnknownName {
                    suggestion: nearest_registered_name(key),
                },
            });
            continue;
        };
        let value = value.into();
        if !spec.value_shape.accepts(&value) {
            diagnostics.push(EnvironmentDiagnostic {
                code: INVALID_VALUE_CODE,
                key: key.to_string(),
                kind: EnvironmentDiagnosticKind::InvalidValue {
                    expected: spec.value_shape,
                },
            });
        }
    }
    diagnostics.sort_by(|left, right| left.key.cmp(&right.key));
    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(EnvironmentValidationError { diagnostics })
    }
}

fn registered_names() -> &'static [&'static str] {
    static NAMES: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    NAMES
        .get_or_init(|| {
            REGISTERED_NAMES
                .lines()
                .filter(|name| !name.is_empty())
                .collect()
        })
        .as_slice()
}

fn spec_for_registered_name(name: &str) -> EnvironmentVariableSpec {
    EnvironmentVariableSpec {
        name: name.to_string(),
        consumer: consumer_for(name),
        value_shape: value_shape_for(name),
        sensitivity: sensitivity_for(name),
    }
}

fn consumer_for(name: &str) -> EnvironmentConsumer {
    if name.starts_with("HARN_TEST_") || name.contains("_TEST_") || name.starts_with("HARN_E2E_") {
        EnvironmentConsumer::TestHarness
    } else if [
        "HARN_BUILD_",
        "HARN_CARGO_",
        "HARN_CHECK_",
        "HARN_CI_",
        "HARN_CODEGEN_",
        "HARN_DEV_",
        "HARN_RELEASE_",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
    {
        EnvironmentConsumer::BuildTooling
    } else if name.starts_with("HARN_CLI_")
        || name.starts_with("HARN_DOCTOR_")
        || name.starts_with("HARN_INIT_")
    {
        EnvironmentConsumer::Cli
    } else {
        EnvironmentConsumer::Runtime
    }
}

fn value_shape_for(name: &str) -> EnvironmentValueShape {
    match name {
        "HARN_LLM_TIMEOUT"
        | "HARN_LLM_IDLE_TIMEOUT"
        | "HARN_LLM_FIRST_TOKEN_TIMEOUT"
        | "HARN_MAX_CONCURRENCY"
        | "HARN_RETENTION_DAYS"
        | "HARN_TOKEN_BUDGET"
        | "HARN_EVENT_LOG_QUEUE_DEPTH" => EnvironmentValueShape::UnsignedInteger,
        "HARN_BUDGET_USD" => EnvironmentValueShape::NonNegativeNumber,
        "HARN_OTEL_SAMPLE_RATIO" => EnvironmentValueShape::UnitInterval,
        "HARN_BYTECODE_CACHE"
        | harn_parser::HARN_LEGACY_AMBIENT_CAPABILITIES_ENV
        | "HARN_LLM_STREAM"
        | "HARN_REPLAY_ENABLED"
        | "HARN_REQUIRE_SIGNED_SKILLS"
        | "HARN_TRACE"
        | "HARN_VERBOSE_CONFIG" => EnvironmentValueShape::Boolean,
        _ => EnvironmentValueShape::OwnerValidated,
    }
}

fn sensitivity_for(name: &str) -> EnvironmentSensitivity {
    if [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "API_KEY",
        "OAUTH_KEY",
        "HEADERS",
        "PRIVATE_KEY",
    ]
    .iter()
    .any(|fragment| name.contains(fragment))
    {
        EnvironmentSensitivity::Credential
    } else {
        EnvironmentSensitivity::Public
    }
}

/// Downstream embedders own this one explicit namespace. A nonempty
/// uppercase-identifier suffix prevents `HARN_EXT_` from becoming a blanket
/// bypass for malformed names.
fn is_extension_name(name: &str) -> bool {
    name.strip_prefix("HARN_EXT_")
        .is_some_and(is_upper_identifier)
}

/// Runtime-generated families have a structural grammar instead of a broad
/// prefix exception. This admits model-role, rate-limit, and secret-provider
/// keys without accepting near-miss fixed keys such as `HARN_LLM_TIMOUT`.
fn is_structured_runtime_name(name: &str) -> bool {
    is_secret_name(name)
        || is_rate_limit_name(name)
        || is_model_role_name(name)
        || is_agent_model_option_name(name)
}

fn is_secret_name(name: &str) -> bool {
    name.strip_prefix("HARN_SECRET_")
        .is_some_and(is_upper_identifier)
}

fn is_rate_limit_name(name: &str) -> bool {
    let Some(suffix) = name.strip_prefix("HARN_RATE_LIMIT_") else {
        return false;
    };
    let Some((provider, field)) = suffix.rsplit_once('_') else {
        return false;
    };
    is_upper_identifier(provider) && matches!(field, "QUEUE" | "RPM" | "TPM" | "CONCURRENCY")
}

fn is_model_role_name(name: &str) -> bool {
    let suffix = name
        .strip_prefix("HARN_LLM_ROLE_")
        .or_else(|| name.strip_prefix("HARN_LLM_"));
    let Some(suffix) = suffix else {
        return false;
    };
    ["_MODEL", "_PROVIDER", "_ROUTE_POLICY"]
        .iter()
        .find_map(|ending| suffix.strip_suffix(ending))
        .is_some_and(is_upper_identifier)
}

/// `std/agent/options` derives role-specific configuration keys from a role
/// token and a closed suffix vocabulary. Keep that dynamic reader family
/// structural so custom roles do not require per-role registry entries.
fn is_agent_model_option_name(name: &str) -> bool {
    const SUFFIXES: &[&str] = &[
        "_EFFORT",
        "_MODEL",
        "_MODEL_ROLE",
        "_PROVIDER",
        "_REASONING_TASK",
        "_TOOL_FORMAT",
    ];
    let Some(prefix) = SUFFIXES.iter().find_map(|suffix| name.strip_suffix(suffix)) else {
        return false;
    };
    let Some(prefix) = prefix.strip_prefix("HARN_") else {
        return false;
    };
    let role = prefix
        .strip_prefix("AGENT_")
        .or_else(|| prefix.strip_prefix("LLM_"))
        .unwrap_or(prefix);
    matches!(prefix, "AGENT" | "LLM") || is_upper_identifier(role)
}

fn is_upper_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        && !value.starts_with('_')
        && !value.ends_with('_')
}

fn nearest_registered_name(name: &str) -> Option<String> {
    registered_names()
        .iter()
        .copied()
        .filter(|candidate| !candidate.ends_with('_'))
        .map(|candidate| (strsim::levenshtein(name, candidate), candidate))
        .min_by_key(|(distance, candidate)| (*distance, *candidate))
        .filter(|(distance, _)| *distance <= 3)
        .map(|(_, candidate)| candidate.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typo_is_typed_and_suggests_registered_name() {
        let error = validate_environment([("HARN_LLM_TIMOUT", "30")]).unwrap_err();
        assert_eq!(
            error.diagnostics(),
            &[EnvironmentDiagnostic {
                code: UNKNOWN_CODE,
                key: "HARN_LLM_TIMOUT".to_string(),
                kind: EnvironmentDiagnosticKind::UnknownName {
                    suggestion: Some("HARN_LLM_TIMEOUT".to_string()),
                },
            }]
        );
    }

    #[test]
    fn known_and_structured_extension_names_are_accepted() {
        validate_environment([
            ("HARN_LLM_TIMEOUT", "30"),
            ("HARN_EXT_ACME_MODE", "custom"),
            ("HARN_LLM_ROLE_REVIEW_MODEL", "reviewer"),
            ("HARN_SECRET_ACME_TOKEN", "credential"),
        ])
        .unwrap();
    }

    #[test]
    fn malformed_extension_name_is_not_a_prefix_bypass() {
        let error = validate_environment([("HARN_EXT_", "anything")]).unwrap_err();
        assert!(matches!(
            error.diagnostics()[0].kind,
            EnvironmentDiagnosticKind::UnknownName { .. }
        ));
    }

    /// These names are composed at read time rather than written literally, so
    /// no source scan can find them and the reverse drift gate cannot see them.
    /// `std/agent/options` builds each key as one of the `HARN_AGENT`,
    /// `HARN_LLM`, `HARN_AGENT_<ROLE>`, `HARN_LLM_<ROLE>`, or `HARN_<ROLE>`
    /// prefixes joined to a closed suffix vocabulary, which is why the grammar
    /// admits a name such as `HARN_LLM_TOOL_FORMAT` that grep alone would read
    /// as a near miss for `HARN_AGENT_TOOL_FORMAT`.
    #[test]
    fn dynamic_agent_role_options_follow_a_closed_structural_grammar() {
        for name in [
            "HARN_AGENT_MODEL",
            "HARN_LLM_TOOL_FORMAT",
            "HARN_AGENT_REVIEW_PROVIDER",
            "HARN_LLM_PLANNER_REASONING_TASK",
            "HARN_RELEASE_EFFORT",
        ] {
            assert!(variable_spec(name).is_some(), "{name}");
        }
        for name in [
            "HARN_AGENT_REVIEW_UNKNOWN",
            "HARN_LLM_TIMOUT",
            "HARN_RELEASE_",
        ] {
            assert!(variable_spec(name).is_none(), "{name}");
        }
    }

    #[test]
    fn credential_metadata_covers_non_api_oauth_keys() {
        assert_eq!(
            variable_spec("HARN_OAUTH_KEY").unwrap().sensitivity,
            EnvironmentSensitivity::Credential
        );
    }

    #[test]
    fn invalid_known_value_is_rejected_at_startup() {
        let error = validate_environment([("HARN_LLM_TIMEOUT", "soon")]).unwrap_err();
        assert_eq!(
            error.diagnostics()[0].kind,
            EnvironmentDiagnosticKind::InvalidValue {
                expected: EnvironmentValueShape::UnsignedInteger
            }
        );
    }

    #[test]
    fn diagnostics_cannot_render_values_even_for_credentials() {
        let secret = "must-never-appear";
        let error = validate_environment([("HARN_CLOUD_API_KEZ", secret)]).unwrap_err();
        let rendered = error.to_string();
        assert!(rendered.contains("HARN_CLOUD_API_KEZ"));
        assert!(!rendered.contains(secret));
    }

    #[test]
    fn registry_is_sorted_unique_and_contains_metadata() {
        let names = registered_names();
        assert!(
            names.windows(2).all(|pair| pair[0] < pair[1]),
            "environment registry must remain sorted and unique"
        );
        let timeout = variable_spec("HARN_LLM_TIMEOUT").unwrap();
        assert_eq!(timeout.consumer, EnvironmentConsumer::Runtime);
        assert_eq!(timeout.value_shape, EnvironmentValueShape::UnsignedInteger);
        let token = variable_spec("HARN_PACKAGE_REGISTRY_TOKEN").unwrap();
        assert_eq!(token.sensitivity, EnvironmentSensitivity::Credential);
        let host_providers = variable_spec(crate::llm_config::HOST_PROVIDERS_CONFIG_ENV).unwrap();
        assert_eq!(host_providers.consumer, EnvironmentConsumer::Runtime);
        assert_eq!(
            host_providers.value_shape,
            EnvironmentValueShape::OwnerValidated
        );
    }

    #[test]
    fn embedded_runtime_bootstrap_accepts_registered_process_environment() {
        crate::initialize_runtime().expect("registered process environment");
    }

    #[test]
    fn every_compiled_harn_name_is_registered_or_structurally_owned() {
        let crates_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("harn-vm lives below crates");
        let mut missing = std::collections::BTreeSet::new();
        for entry in walkdir::WalkDir::new(crates_dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_type().is_file()
                    && entry.path().extension().and_then(OsStr::to_str) == Some("rs")
                    && entry
                        .path()
                        .components()
                        .any(|component| component.as_os_str() == "src")
                    && entry.file_name() != "environment_registry.rs"
            })
        {
            let source = std::fs::read_to_string(entry.path()).expect("read Rust source");
            for token in harn_name_tokens(&source) {
                if variable_spec(token).is_none()
                    && !matches!(token, "HARN_LLM_" | "HARN_LLM_ROLE_" | "HARN_SECRET_")
                {
                    missing.insert(format!("{}: {token}", entry.path().display()));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "compiled HARN_* names missing from environment_registry_names.txt:\n{}",
            missing.into_iter().collect::<Vec<_>>().join("\n")
        );
    }

    #[test]
    fn every_harn_script_owned_name_is_registered_or_structurally_owned() {
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("harn-vm lives below workspace/crates");
        let source_roots = [
            "benchmarks",
            "conformance",
            "crates",
            "evals",
            "examples",
            "experiments",
            "perf",
            "personas",
            "scripts",
            "tests",
        ];
        let mut missing = std::collections::BTreeSet::new();
        for source_root in source_roots {
            let source_root = workspace_root.join(source_root);
            if !source_root.exists() {
                continue;
            }
            for entry in walkdir::WalkDir::new(source_root)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.file_type().is_file()
                        && entry.path().extension().and_then(OsStr::to_str) == Some("harn")
                })
            {
                let source = std::fs::read_to_string(entry.path()).expect("read Harn source");
                for token in harn_name_tokens(&source) {
                    if variable_spec(token).is_none()
                        && !matches!(
                            token,
                            "HARN_AGENT"
                                | "HARN_AGENT_"
                                | "HARN_LLM"
                                | "HARN_LLM_"
                                | "HARN_PLANNER"
                                | "HARN_RELEASE"
                        )
                    {
                        missing.insert(format!("{}: {token}", entry.path().display()));
                    }
                }
            }
        }
        assert!(
            missing.is_empty(),
            "Harn-script HARN_* names missing from environment_registry_names.txt:\n{}",
            missing.into_iter().collect::<Vec<_>>().join("\n")
        );
    }

    /// Registered names that no repository source reads, and why they must
    /// stay. Every entry is a standing exception to the reverse drift gate, so
    /// it carries the reason a reader cannot exist in this tree. An entry whose
    /// name gains a reader is rejected too, which keeps the list from rotting.
    const UNREAD_NAME_ALLOWLIST: &[(&str, &str)] = &[];

    /// Build output, caches, vendored dependencies, and nested checkouts are
    /// not sources of truth for who reads a variable. A nested worktree is the
    /// dangerous one: it carries its own copy of the registry and of every
    /// reader, so walking into it would make any name look alive.
    fn is_pruned_reference_directory(entry: &walkdir::DirEntry) -> bool {
        if !entry.file_type().is_dir() {
            return false;
        }
        let Some(name) = entry.file_name().to_str() else {
            return true;
        };
        name.starts_with(".target")
            || matches!(
                name,
                ".build"
                    | ".burin"
                    | ".git"
                    | ".harn-runs"
                    | ".harn-toolchain-cache"
                    | ".venv"
                    | ".worktrees"
                    | "__pycache__"
                    | "changelog"
                    | "dist"
                    | "node_modules"
                    | "pkg"
                    | "target"
            )
    }

    /// Prose can mention a retired name forever, so release notes and docs do
    /// not count as a reader. The registry itself is excluded for the same
    /// reason the forward gates exclude it: listing a name is not reading it.
    fn is_reference_source(path: &std::path::Path) -> bool {
        if path.extension().and_then(OsStr::to_str) == Some("md") {
            return false;
        }
        !matches!(
            path.file_name().and_then(OsStr::to_str),
            Some("environment_registry.rs" | "environment_registry_names.txt")
        )
    }

    /// Every maximal `HARN_*` token that starts at an identifier boundary.
    ///
    /// The left boundary matters: `FAKE_HARN_RECORD` and `STDLIB_HARN_DIR` are
    /// unrelated local identifiers that merely end in a registered name, and
    /// counting them as readers is what let dead entries look alive.
    #[expect(
        clippy::string_slice,
        reason = "start/end bound an ASCII HARN_* token found by match_indices"
    )]
    fn bounded_harn_tokens(source: &str) -> impl Iterator<Item = &str> {
        let bytes = source.as_bytes();
        source.match_indices("HARN_").filter_map(move |(start, _)| {
            if start > 0 {
                let previous = bytes[start - 1];
                if previous.is_ascii_alphanumeric() || previous == b'_' {
                    return None;
                }
            }
            let mut end = start + "HARN_".len();
            while end < bytes.len()
                && (bytes[end].is_ascii_uppercase()
                    || bytes[end].is_ascii_digit()
                    || bytes[end] == b'_')
            {
                end += 1;
            }
            Some(&source[start..end])
        })
    }

    /// The forward gates only prove that every name a source reads is
    /// registered. Without the reverse direction, a name whose reader is
    /// deleted—or that was never a variable at all—stays registered forever,
    /// keeps passing startup validation, and reads as a supported knob that
    /// silently does nothing.
    #[test]
    fn every_registered_name_is_read_somewhere_in_the_tree() {
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("harn-vm lives below workspace/crates");
        let mut referenced = std::collections::BTreeSet::new();
        for entry in walkdir::WalkDir::new(workspace_root)
            .into_iter()
            .filter_entry(|entry| !is_pruned_reference_directory(entry))
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file() && is_reference_source(entry.path()))
        {
            let Ok(source) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            for token in bounded_harn_tokens(&source) {
                referenced.insert(token.to_string());
            }
        }

        let allowed: std::collections::BTreeSet<&str> = UNREAD_NAME_ALLOWLIST
            .iter()
            .map(|(name, _)| *name)
            .collect();
        let unregistered_allowlist = allowed
            .iter()
            .copied()
            .filter(|name| registered_names().binary_search(name).is_err())
            .collect::<Vec<_>>();
        assert!(
            unregistered_allowlist.is_empty(),
            "allowlisted names are not in environment_registry_names.txt:\n{}",
            unregistered_allowlist.join("\n")
        );
        let readable_allowlist = allowed
            .iter()
            .copied()
            .filter(|name| referenced.contains(*name))
            .collect::<Vec<_>>();
        assert!(
            readable_allowlist.is_empty(),
            "allowlisted names now have readers; drop them from UNREAD_NAME_ALLOWLIST:\n{}",
            readable_allowlist.join("\n")
        );

        let unread = registered_names()
            .iter()
            .copied()
            .filter(|name| !referenced.contains(*name) && !allowed.contains(name))
            .collect::<Vec<_>>();
        assert!(
            unread.is_empty(),
            "registered names no source reads; delete them from \
             environment_registry_names.txt or allowlist them with a reason:\n{}",
            unread.join("\n")
        );
    }

    #[expect(
        clippy::string_slice,
        reason = "start/end bound an ASCII HARN_* token found by match_indices"
    )]
    fn harn_name_tokens(source: &str) -> impl Iterator<Item = &str> {
        source.match_indices("\"HARN_").filter_map(|(quote, _)| {
            let start = quote + 1;
            let bytes = source.as_bytes();
            let mut end = start + "HARN_".len();
            while end < bytes.len()
                && (bytes[end].is_ascii_uppercase()
                    || bytes[end].is_ascii_digit()
                    || bytes[end] == b'_')
            {
                end += 1;
            }
            (end > start + "HARN_".len()).then(|| &source[start..end])
        })
    }
}
