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
    Enumerated(&'static [&'static str]),
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
                    "{}: unknown Harn environment variable `{}`.",
                    self.code, self.key
                )?;
                if let Some(suggestion) = suggestion {
                    write!(formatter, " Did you mean `{suggestion}`?")?;
                }
                formatter.write_str(" Use `HARN_EXT_<NAME>` for settings owned by a calling tool.")
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
    fn description(self) -> String {
        match self {
            Self::OwnerValidated => "valid for its owning subsystem".to_string(),
            Self::Boolean => {
                "a boolean (`true`, `false`, `yes`, `no`, `on`, `off`, `1`, or `0`)".to_string()
            }
            Self::UnsignedInteger => "an unsigned integer".to_string(),
            Self::NonNegativeNumber => "a non-negative number".to_string(),
            Self::UnitInterval => "a number between 0 and 1".to_string(),
            Self::Enumerated(values) => format!(
                "one of {}",
                values
                    .iter()
                    .map(|value| format!("`{value}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
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
            Self::Enumerated(values) => {
                let value = value.trim().to_ascii_lowercase();
                values.contains(&value.as_str())
            }
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
    } else if matches!(
        name,
        "HARN_CLI_AOT_GEN_BIN"
            | "HARN_ENUM_COMPILER_ARCHIVE_STAGE"
            | "HARN_LSP_INSTALL_ATTEMPTS"
            | "HARN_RATE_LIMIT_EXHAUSTED_PERCENT"
            | "HARN_RATE_LIMIT_MAX_WAIT_SECONDS"
            | "HARN_WASM_PACK"
    ) || [
        "HARN_BUILD_",
        "HARN_CARGO_",
        "HARN_CHECK_",
        "HARN_CI_",
        "HARN_CLI_ARTIFACT_",
        "HARN_CODEGEN_",
        "HARN_DEV_",
        "HARN_GC_",
        "HARN_HOOKS_",
        "HARN_PERSISTENT_TARGET_",
        "HARN_PREPUSH_",
        "HARN_RELEASE_",
        "HARN_TARGET_GC_",
        "HARN_WARM_",
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
        | "HARN_RETENTION_DAYS"
        | "HARN_EVENT_LOG_QUEUE_DEPTH"
        | "HARN_LSP_REQUEST_TIMEOUT_MS"
        | "HARN_LSP_STARTUP_TIMEOUT_MS" => EnvironmentValueShape::UnsignedInteger,
        "HARN_OTEL_SAMPLE_RATIO" => EnvironmentValueShape::UnitInterval,
        "HARN_ALLOW_TOOLCHAIN_MISMATCH"
        | "HARN_BYTECODE_CACHE"
        | "HARN_DISABLE_CODE_INDEX_WARM"
        | "HARN_DISPATCH_GENERATION_DEBUG"
        | "HARN_FLIGHT_RECORDER"
        | harn_parser::HARN_LEGACY_AMBIENT_CAPABILITIES_ENV
        | "HARN_LLM_STREAM"
        | "HARN_REPLAY_ENABLED"
        | "HARN_REQUIRE_SIGNED_SKILLS"
        | "HARN_TRACE"
        | "HARN_VERBOSE_CONFIG" => EnvironmentValueShape::Boolean,
        "HARN_REFACTOR_BACKEND" => {
            EnvironmentValueShape::Enumerated(&["auto", "language_server", "syntax_tree"])
        }
        crate::vm::subtask::PLACEMENT_ENV => {
            EnvironmentValueShape::Enumerated(crate::vm::subtask::PLACEMENT_VALUES)
        }
        _ => EnvironmentValueShape::OwnerValidated,
    }
}

pub(crate) fn sensitivity_for(name: &str) -> EnvironmentSensitivity {
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
    fn subtask_placement_uses_the_runtime_owned_closed_vocabulary() {
        let spec = variable_spec(crate::vm::subtask::PLACEMENT_ENV).unwrap();
        assert_eq!(
            spec.value_shape,
            EnvironmentValueShape::Enumerated(crate::vm::subtask::PLACEMENT_VALUES)
        );
        validate_environment([(crate::vm::subtask::PLACEMENT_ENV, "worker")]).unwrap();
        validate_environment([(crate::vm::subtask::PLACEMENT_ENV, "CURRENT_THREAD")]).unwrap();

        let error =
            validate_environment([(crate::vm::subtask::PLACEMENT_ENV, "workers")]).unwrap_err();
        assert_eq!(
            error.diagnostics()[0].kind,
            EnvironmentDiagnosticKind::InvalidValue {
                expected: EnvironmentValueShape::Enumerated(crate::vm::subtask::PLACEMENT_VALUES)
            }
        );
        assert!(error.to_string().contains("`worker`, `current_thread`"));
    }

    #[test]
    fn diagnostics_cannot_render_values_even_for_credentials() {
        let secret = "must-never-appear";
        let error = validate_environment([("HARN_CLOUD_API_KEZ", secret)]).unwrap_err();
        let rendered = error.to_string();
        assert!(rendered.contains("HARN_CLOUD_API_KEZ"));
        assert!(!rendered.contains(secret));
    }

    /// Dispatch refuses the retired budget names, so a run cannot treat them
    /// as a live ceiling. Names are assembled without a `"HARN_` token so this
    /// fixture is not a live reader.
    #[test]
    fn retired_limit_names_are_unknown_at_startup() {
        for name in [
            concat!("HARN", "_BUDGET_USD"),
            concat!("HARN", "_TOKEN_BUDGET"),
            concat!("HARN", "_MAX_CONCURRENCY"),
            concat!("HARN", "_NETWORK_MODE"),
            concat!("HARN", "_FILESYSTEM_MODE"),
            concat!("HARN", "_SANDBOX_MODE"),
        ] {
            let error = validate_environment([(name, "1")]).expect_err(name);
            assert!(
                error.to_string().contains(name),
                "startup must name {name}, got {error}"
            );
            assert!(
                variable_spec(name).is_none(),
                "{name} must stay unregistered"
            );
        }
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
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("harn-vm lives below workspace/crates");
        let protocol_symbols = protocol_artifact_symbol_names(workspace_root);
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
                    && !is_protocol_artifact_projection(entry.path())
                    && entry.file_name() != "environment_registry.rs"
            })
        {
            let source = std::fs::read_to_string(entry.path()).expect("read Rust source");
            for token in harn_name_tokens(&source) {
                if variable_spec(token).is_none()
                    && !protocol_symbols.contains(token)
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
        let protocol_symbols = protocol_artifact_symbol_names(workspace_root);
        let mut missing = std::collections::BTreeSet::new();
        for source_root in source_roots {
            let source_root = workspace_root.join(source_root);
            if !source_root.exists() {
                continue;
            }
            for entry in walkdir::WalkDir::new(source_root)
                .into_iter()
                .filter_entry(|entry| !is_pruned_reference_directory(entry))
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.file_type().is_file()
                        && entry.path().extension().and_then(OsStr::to_str) == Some("harn")
                })
            {
                let source = std::fs::read_to_string(entry.path()).expect("read Harn source");
                for token in harn_name_tokens(&source) {
                    // Bare prefixes, not names. Each is a literal that a script
                    // concatenates a suffix onto, so the scanner sees the
                    // prefix alone and cannot resolve the name that is actually
                    // read. The names those expressions produce are registered
                    // individually; only the unresolvable fragment is excused.
                    if variable_spec(token).is_none()
                        && !protocol_symbols.contains(token)
                        && !matches!(
                            token,
                            "HARN_AGENT"
                                | "HARN_AGENT_"
                                | "HARN_BOOTSTRAP_"
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

    /// Conservatively census executable CI references, including inherited
    /// reads and inline child assignments. Repository variables, comments,
    /// unexported shell constants and literal workflow-command labels are not
    /// process-environment inputs.
    fn ci_environment_names<'a>(
        path: &std::path::Path,
        source: &'a str,
    ) -> std::collections::BTreeSet<&'a str> {
        let mut names = std::collections::BTreeSet::new();
        let mut constants = std::collections::BTreeSet::new();
        let workflow = matches!(
            path.extension().and_then(OsStr::to_str),
            Some("yml" | "yaml")
        );
        let mut run_indent = None;
        let exported: std::collections::BTreeSet<_> = source
            .lines()
            .filter_map(|line| line.trim_start().strip_prefix("export "))
            .flat_map(bounded_harn_tokens)
            .collect();
        for line in source.lines() {
            let trimmed = line.trim_start();
            let indent = line.len() - trimmed.len();
            if !trimmed.is_empty() && run_indent.is_some_and(|base| indent <= base) {
                run_indent = None;
            }
            let mapping = trimmed.strip_prefix("- ").unwrap_or(trimmed).trim_start();
            if workflow
                && run_indent.is_none()
                && (mapping.starts_with("run:")
                    || mapping.starts_with("\"run\":")
                    || mapping.starts_with("'run':"))
            {
                run_indent = Some(line.len() - mapping.len());
            }
            if trimmed.starts_with('#') || trimmed.starts_with("//") {
                continue;
            }
            let constant = (path.extension().and_then(OsStr::to_str) == Some("sh"))
                .then(|| trimmed.strip_prefix("readonly "))
                .flatten()
                .and_then(|binding| binding.split_once('='))
                .map(|(name, _)| name.trim())
                .filter(|name| name.starts_with("HARN_") && !exported.contains(name));
            let diagnostic = trimmed
                .strip_prefix("echo ")
                .or_else(|| trimmed.strip_prefix("printf "))
                .map(str::trim_start)
                .map(|text| text.trim_start_matches(['\'', '"']))
                .and_then(|text| text.strip_prefix("::"))
                .and_then(|text| text.split([' ', ':']).next())
                .is_some_and(|kind| matches!(kind, "notice" | "warning" | "error" | "debug"));
            for (start, end) in harn_token_spans(line) {
                let name = line.get(start..end).expect("ASCII name boundaries");
                let before = line.get(..start).expect("ASCII name boundary");
                let process_read =
                    before.ends_with("env.") || before.to_ascii_lowercase().ends_with("$env:");
                let expanded = before.ends_with('$') || before.ends_with("${") || process_read;
                let repository_variable = workflow
                    && before.ends_with("vars.")
                    && (run_indent.is_none()
                        || before
                            .rsplit_once("${{")
                            .is_some_and(|(_, expression)| !expression.contains("}}")));
                if name.ends_with('_')
                    || repository_variable
                    || (constants.contains(name) && !process_read)
                    || (constant == Some(name) && before.trim() == "readonly")
                    || (diagnostic && !expanded)
                {
                    continue;
                }
                names.insert(name);
            }
            if let Some(constant) = constant {
                constants.insert(constant);
            }
        }
        names
    }

    struct CiEnvironmentCensus<'a> {
        observed: std::collections::BTreeSet<&'a str>,
        missing: std::collections::BTreeSet<&'a str>,
    }

    fn ci_environment_census<'a>(
        path: &std::path::Path,
        source: &'a str,
    ) -> CiEnvironmentCensus<'a> {
        let observed: std::collections::BTreeSet<_> = environment_reference_tokens(path, source)
            .into_iter()
            .collect();
        let missing = observed
            .iter()
            .copied()
            .filter(|name| variable_spec(name).is_none())
            .collect();
        CiEnvironmentCensus { observed, missing }
    }

    #[test]
    fn ci_name_scan_reads_inputs_without_registering_non_environment_symbols() {
        let source = concat!(
            "HARN_CI_ASSIGNED=1 child\n",
            "echo \"$HARN_BIN ${HARN_CI_INHERITED:-1}\"\n",
            "process.env.HARN_CI_DOT_READ\n",
            "os.environ['HARN_CI_STRING_READ']\n",
            "$env:HARN_CI_POWERSHELL_READ\n",
            "${{ env.HARN_CI_WORKFLOW_READ }}\n",
            "readonly HARN_CI_LOCAL_CONSTANT=7\n",
            "echo \"$HARN_CI_LOCAL_CONSTANT\"\n",
            "readonly HARN_CI_DEFAULTED=\"${HARN_CI_DEFAULTED:-1}\"\n",
            "readonly HARN_CI_EXPORTED=7\nexport HARN_CI_EXPORTED\n",
            "echo \"::notice::HARN_CI_LOG_LABEL=value\"\n",
            "echo \"::notice::$HARN_CI_LOG_READ\"\n",
            "# HARN_CI_COMMENT_ONLY=1\n",
            "// HARN_CI_COMMENT_ONLY\n",
        );
        let names: std::collections::BTreeSet<_> =
            environment_reference_tokens(std::path::Path::new("scripts/ci/probe.sh"), source)
                .into_iter()
                .collect();
        for name in [
            "HARN_BIN",
            "HARN_CI_ASSIGNED",
            "HARN_CI_INHERITED",
            "HARN_CI_DOT_READ",
            "HARN_CI_STRING_READ",
            "HARN_CI_POWERSHELL_READ",
            "HARN_CI_WORKFLOW_READ",
            "HARN_CI_LOG_READ",
            "HARN_CI_DEFAULTED",
            "HARN_CI_EXPORTED",
        ] {
            assert!(names.contains(name), "missed input {name}");
        }
        assert!(
            variable_spec("HARN_BIN").is_some(),
            "known registered positive control"
        );
        assert!(
            variable_spec("HARN_CI_DOT_READ").is_none(),
            "unregistered read must remain a refusal"
        );
        for name in [
            "HARN_CI_LOCAL_CONSTANT",
            "HARN_CI_LOG_LABEL",
            "HARN_CI_COMMENT_ONLY",
        ] {
            assert!(!names.contains(name), "non-environment symbol {name}");
        }
        let workflow = concat!(
            "if: vars.HARN_CI_REPOSITORY_SETTING == 'on'\n",
            "steps:\n  - run: |\n",
            "      echo \"$HARN_CI_SCRIPT_READ\"\n",
            "      node -e 'const vars=process.env; vars.HARN_CI_JS_ALIAS'\n",
            "      echo \"${{ vars.HARN_CI_REPOSITORY_SETTING }}\"\n",
        );
        let names: std::collections::BTreeSet<_> = environment_reference_tokens(
            std::path::Path::new(".github/workflows/probe.yml"),
            workflow,
        )
        .into_iter()
        .collect();
        assert!(names.contains("HARN_CI_SCRIPT_READ"));
        assert!(
            names.contains("HARN_CI_JS_ALIAS"),
            "script aliases are not GitHub context variables"
        );
        assert!(!names.contains("HARN_CI_REPOSITORY_SETTING"));
        for name in ["HARN_WINDOWS_DEVDRIVE", "HARN_WINDOWS_LLD"] {
            assert!(
                variable_spec(name).is_none(),
                "repository-only variables are not process-environment settings"
            );
        }
        let path = std::path::Path::new("scripts/ci/probe.sh");
        let refused = ci_environment_census(path, "echo $HARN_CI_DOT_READ");
        assert_eq!(refused.observed.len(), 1);
        assert_eq!(
            refused.missing.into_iter().collect::<Vec<_>>(),
            vec!["HARN_CI_DOT_READ"]
        );
        let admitted = ci_environment_census(path, "echo $HARN_BIN");
        assert_eq!(admitted.observed.len(), 1);
        assert!(
            admitted.missing.is_empty(),
            "a measured zero after the refusal control"
        );
    }

    #[test]
    fn every_ci_environment_name_is_registered_or_structurally_owned() {
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("harn-vm lives below workspace/crates");
        let mut observed = std::collections::BTreeSet::new();
        let mut missing = std::collections::BTreeSet::new();
        for root in ["scripts/ci", ".github/workflows"] {
            let directory = workspace_root.join(root);
            assert!(
                directory.is_dir(),
                "missing CI owner {}",
                directory.display()
            );
            let mut scanned = 0;
            for entry in walkdir::WalkDir::new(directory)
                .into_iter()
                .filter_entry(|entry| !is_pruned_reference_directory(entry))
            {
                let entry = entry.expect("walk every CI contract owner");
                if !entry.file_type().is_file() || !is_reference_source(entry.path()) {
                    continue;
                }
                let source = std::fs::read_to_string(entry.path()).expect("read CI source");
                scanned += 1;
                let census = ci_environment_census(entry.path(), &source);
                for name in census.missing {
                    missing.insert(format!("{}: {name}", entry.path().display()));
                }
                observed.extend(census.observed.into_iter().map(str::to_owned));
            }
            assert!(scanned > 0, "no executable CI sources measured in {root}");
        }
        assert!(
            observed.contains("HARN_BIN"),
            "known registered positive control"
        );
        assert!(
            observed.contains("HARN_PERSISTENT_TARGET_MAX_GIB"),
            "must census the read that broke release smoke"
        );
        assert!(
            missing.is_empty(),
            "CI-owned HARN_* names missing from \
             environment_registry_names.txt:\n{}",
            missing.into_iter().collect::<Vec<_>>().join("\n")
        );
    }

    #[test]
    fn public_hook_policy_names_are_registered() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("harn-vm lives below workspace/crates");
        let mut names = std::collections::BTreeSet::new();
        for path in [".githooks/lib.sh", ".githooks/pre-push"] {
            let source = std::fs::read_to_string(root.join(path))
                .expect("read the public hook policy owner");
            names.extend(
                bounded_harn_tokens(&source)
                    .filter(|name| {
                        name.starts_with("HARN_HOOKS_") || name.starts_with("HARN_PREPUSH_")
                    })
                    .map(str::to_string),
            );
        }
        assert!(
            names.contains("HARN_HOOKS_FULL_LOCAL"),
            "the hook policy census must measure its established opt-in switch"
        );
        for name in names {
            let spec =
                variable_spec(&name).unwrap_or_else(|| panic!("unregistered hook setting {name}"));
            assert_eq!(spec.consumer, EnvironmentConsumer::BuildTooling, "{name}");
        }
    }

    #[test]
    fn target_cache_collector_names_are_registered() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("harn-vm lives below workspace/crates");
        let mut names = std::collections::BTreeSet::new();
        for script in [
            "scripts/prune_stale_targets.sh",
            "scripts/target_gc_maintenance.sh",
            "scripts/ci/bound_persistent_target.sh",
        ] {
            let source = std::fs::read_to_string(root.join(script)).expect("read target collector");
            names.extend(
                source
                    .split(|c: char| !c.is_ascii_uppercase() && !c.is_ascii_digit() && c != '_')
                    .filter(|name| {
                        (name.starts_with("HARN_TARGET_GC_")
                            || name.starts_with("HARN_GC_")
                            || name.starts_with("HARN_PERSISTENT_TARGET_"))
                            && !name.ends_with('_')
                    })
                    .map(str::to_owned),
            );
        }
        assert!(
            names.len() >= 12,
            "target collector name census is incomplete"
        );
        for name in names {
            let spec = variable_spec(&name)
                .unwrap_or_else(|| panic!("unregistered collector name {name}"));
            assert_eq!(spec.consumer, EnvironmentConsumer::BuildTooling, "{name}");
        }
    }

    /// Registered names that have no repository-owned reference, and why they must
    /// stay. Every entry is a standing exception to the reverse drift gate, so
    /// it carries the reason a source reference cannot exist in this tree. An
    /// entry that gains an owner is rejected too, which keeps the list from
    /// rotting.
    const UNREAD_NAME_ALLOWLIST: &[(&str, &str)] = &[];

    /// Build output, caches, vendored dependencies, and nested checkouts are
    /// not sources of truth for variable ownership. A nested worktree is the
    /// dangerous one: it carries its own copy of the registry and literals, so
    /// walking into it would make any name look alive.
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
                    | ".harn"
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

    #[test]
    fn generated_harn_state_is_not_an_environment_contract_owner() {
        let root = tempfile::tempdir().expect("temporary workspace");
        let state = root.path().join(".harn");
        std::fs::create_dir(&state).expect("create generated Harn state");
        let entry = walkdir::WalkDir::new(root.path())
            .into_iter()
            .filter_map(Result::ok)
            .find(|entry| entry.path() == state)
            .expect("walk generated Harn state");

        assert!(is_pruned_reference_directory(&entry));
    }

    /// Generated protocol names describe wire contracts; they are not process
    /// environment reads. Exclude both their generator and generated output so
    /// a public constant cannot accidentally keep an environment variable alive.
    fn is_protocol_artifact_projection(path: &std::path::Path) -> bool {
        path.components().any(|component| {
            matches!(
                component.as_os_str().to_str(),
                Some("dump_protocol_artifacts" | "protocol-artifacts")
            )
        })
    }

    /// Prose can mention a retired name forever, so release notes and docs do
    /// not count as owners. Registries and protocol projections are excluded
    /// for the same reason: listing a name is not an environment contract.
    fn is_reference_source(path: &std::path::Path) -> bool {
        if path.extension().and_then(OsStr::to_str) == Some("md")
            || is_protocol_artifact_projection(path)
        {
            return false;
        }
        !matches!(
            path.file_name().and_then(OsStr::to_str),
            Some("environment_registry.rs" | "environment_registry_names.txt")
        )
    }

    /// The generated Rust projection is the authority for public protocol
    /// symbol names. Deriving this set keeps the environment registry from
    /// maintaining a second list of `HARN_*` constants.
    fn protocol_artifact_symbol_names(
        workspace_root: &std::path::Path,
    ) -> std::collections::BTreeSet<String> {
        let path = workspace_root.join("spec/protocol-artifacts/harn-protocol.rs");
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        bounded_harn_tokens(&source).map(str::to_string).collect()
    }

    #[test]
    fn protocol_artifact_names_are_not_environment_references() {
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("harn-vm lives below workspace/crates");
        let protocol_symbols = protocol_artifact_symbol_names(workspace_root);
        let generator = std::path::Path::new(
            "crates/harn-cli/src/commands/dump_protocol_artifacts/typescript.rs",
        );
        let projection = std::path::Path::new("spec/protocol-artifacts/harn-protocol.ts");
        let runtime = std::path::Path::new("crates/harn-vm/src/llm/call.rs");

        assert!(is_protocol_artifact_projection(generator));
        assert!(is_protocol_artifact_projection(projection));
        assert!(!is_reference_source(generator));
        assert!(!is_reference_source(projection));
        assert!(is_reference_source(runtime));
        assert!(variable_spec("HARN_AGENT_EVENT_KINDS").is_none());
        assert!(variable_spec("HARN_WORKER_STATUSES").is_none());
        assert!(protocol_symbols.contains("HARN_AGENT_EVENT_KINDS"));
        assert!(!protocol_symbols.contains("HARN_ACP_VERBOSE"));
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum EnvironmentReferenceSyntax {
        StringLiteral,
        ShellLike,
    }

    fn environment_reference_syntax(path: &std::path::Path) -> EnvironmentReferenceSyntax {
        let file_name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
        let extension = path.extension().and_then(OsStr::to_str).unwrap_or_default();
        if matches!(file_name, "Makefile" | "GNUmakefile")
            || file_name.starts_with(".env")
            || matches!(
                extension,
                "bash" | "bat" | "cmd" | "fish" | "mk" | "ps1" | "sh" | "yaml" | "yml" | "zsh"
            )
        {
            EnvironmentReferenceSyntax::ShellLike
        } else {
            EnvironmentReferenceSyntax::StringLiteral
        }
    }

    fn environment_reference_tokens<'a>(path: &std::path::Path, source: &'a str) -> Vec<&'a str> {
        // Forward registration and reverse ownership use the same CI reader.
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("harn-vm lives below workspace/crates");
        let relative = path.strip_prefix(workspace_root).unwrap_or(path);
        if relative.starts_with("scripts/ci") || relative.starts_with(".github/workflows") {
            return ci_environment_names(path, source).into_iter().collect();
        }
        match environment_reference_syntax(path) {
            EnvironmentReferenceSyntax::StringLiteral => harn_name_tokens(source).collect(),
            EnvironmentReferenceSyntax::ShellLike => bounded_harn_tokens(source).collect(),
        }
    }

    /// Every maximal `HARN_*` token that starts at an identifier boundary.
    /// Shell, Make, and YAML reference variables without quoting their names;
    /// compiled languages use the literal-only scanner instead.
    fn bounded_harn_tokens(source: &str) -> impl Iterator<Item = &str> {
        harn_token_spans(source).filter_map(|(start, end)| source.get(start..end))
    }

    fn harn_token_spans(source: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
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
            Some((start, end))
        })
    }

    /// The forward gates prove that every environment-shaped source reference
    /// is registered. The reverse direction rejects rows whose owner vanished,
    /// while generated protocol identifiers cannot keep unrelated environment
    /// knobs alive in compiled languages.
    #[test]
    fn every_registered_name_has_a_non_projection_source_reference() {
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("harn-vm lives below workspace/crates");
        let mut referenced = std::collections::BTreeSet::new();
        let protocol_symbols = protocol_artifact_symbol_names(workspace_root);
        for entry in walkdir::WalkDir::new(workspace_root)
            .into_iter()
            .filter_entry(|entry| !is_pruned_reference_directory(entry))
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file() && is_reference_source(entry.path()))
        {
            let Ok(source) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            for token in environment_reference_tokens(entry.path(), &source) {
                if !protocol_symbols.contains(token) {
                    referenced.insert(token.to_string());
                }
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
        let owned_allowlist = allowed
            .iter()
            .copied()
            .filter(|name| referenced.contains(*name))
            .collect::<Vec<_>>();
        assert!(
            owned_allowlist.is_empty(),
            "allowlisted names now have source owners; drop them from UNREAD_NAME_ALLOWLIST:\n{}",
            owned_allowlist.join("\n")
        );

        let unread = registered_names()
            .iter()
            .copied()
            .filter(|name| !referenced.contains(*name) && !allowed.contains(name))
            .collect::<Vec<_>>();
        assert!(
            unread.is_empty(),
            "registered names have no source owner; delete them from \
             environment_registry_names.txt or allowlist them with a reason:\n{}",
            unread.join("\n")
        );
    }

    #[test]
    fn environment_reference_scan_dispatches_by_source_syntax() {
        let compiled = concat!(
            "const HARN_AGENT_EVENT_KINDS: &[&str] = &[];\n",
            "const ENV: &str = \"HARN_REAL_ENVIRONMENT_KNOB\";\n",
        );
        assert_eq!(
            environment_reference_tokens(std::path::Path::new("runtime.rs"), compiled),
            vec!["HARN_REAL_ENVIRONMENT_KNOB"]
        );
        assert_eq!(
            environment_reference_tokens(
                std::path::Path::new("bench.sh"),
                "cache=${HARN_BENCH_CACHE_DIR:-target}\n",
            ),
            vec!["HARN_BENCH_CACHE_DIR"]
        );
        assert_eq!(
            environment_reference_tokens(
                std::path::Path::new("Makefile"),
                "HARN_BIN_ASSIGN = harn_bin\n",
            ),
            vec!["HARN_BIN_ASSIGN"]
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
