//! Config loading and per-run override machinery: the process-wide cached
//! config, env/home overlays, the runtime-catalog overlay, thread-local user
//! overrides, and the compiled-in default catalog.
use std::cell::RefCell;
use std::sync::{OnceLock, RwLock};

use super::*;

static CONFIG: OnceLock<ProvidersConfig> = OnceLock::new();
static CONFIG_PATH: OnceLock<String> = OnceLock::new();
static RUNTIME_CATALOG_OVERLAY: OnceLock<RwLock<Option<ProvidersConfig>>> = OnceLock::new();

thread_local! {
    /// Thread-local provider config overlays installed by the CLI after it
    /// reads the nearest `harn.toml` plus any installed package manifests.
    /// Kept thread-local so tests and multi-VM hosts can scope extensions to
    /// the current run without mutating the process-wide default config.
    static USER_OVERRIDES: RefCell<Option<ProvidersConfig>> = const { RefCell::new(None) };
}

/// Load and cache the providers config. Called once at VM startup.
pub fn load_config() -> &'static ProvidersConfig {
    CONFIG.get_or_init(|| {
        let mut config = default_config();
        let verbose_config_logging = matches!(
            std::env::var("HARN_VERBOSE_CONFIG").ok().as_deref(),
            Some("1" | "true" | "TRUE" | "yes" | "YES")
        ) || matches!(
            std::env::var("HARN_ACP_VERBOSE").ok().as_deref(),
            Some("1" | "true" | "TRUE" | "yes" | "YES")
        );
        if let Ok(path) = std::env::var("HARN_PROVIDERS_CONFIG") {
            if let Some(overlay) = read_external_config(&path, verbose_config_logging) {
                config.merge_from(&overlay);
                let _ = CONFIG_PATH.set(path);
                return config;
            }
        }
        if should_load_home_config() {
            if let Some(home) = dirs_or_home() {
                let path = format!("{home}/.config/harn/providers.toml");
                if let Some(overlay) = read_external_config(&path, false) {
                    config.merge_from(&overlay);
                    let _ = CONFIG_PATH.set(path);
                    return config;
                }
            }
        }
        config
    })
}

fn read_external_config(path: &str, verbose: bool) -> Option<ProvidersConfig> {
    match std::fs::read_to_string(path) {
        // Single parse entry point (`parse_config_toml`) so every overlay
        // layer — `HARN_PROVIDERS_CONFIG`, the home file, `[llm]` manifest
        // sections — honors the same schema, including `[patch.models]`.
        Ok(content) => match parse_config_toml_with_diagnostics(&content) {
            Ok(parsed) => {
                for diagnostic in &parsed.diagnostics {
                    eprintln!("[llm_config] warning in {path}: {diagnostic}");
                }
                if verbose {
                    eprintln!(
                        "[llm_config] Loaded {} providers, {} aliases from {}",
                        parsed.config.providers.len(),
                        parsed.config.aliases.len(),
                        path
                    );
                }
                Some(parsed.config)
            }
            Err(error) => {
                eprintln!("[llm_config] TOML parse error in {path}: {error}");
                None
            }
        },
        Err(error) => {
            if verbose {
                eprintln!("[llm_config] Cannot read {path}: {error}");
            }
            None
        }
    }
}

fn should_load_home_config() -> bool {
    // Unit tests should cover embedded defaults plus explicit overlays, not
    // whichever provider file happens to exist on the developer machine.
    !cfg!(test)
}

/// Parse a provider/model catalog overlay in the same shape as
/// `providers.toml` or `[llm]` package-manifest sections.
pub fn parse_config_toml(src: &str) -> Result<ProvidersConfig, toml::de::Error> {
    parse_config_toml_with_diagnostics(src).map(|parsed| parsed.config)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConfigDiagnostic {
    pub path: String,
    pub message: String,
}

impl std::fmt::Display for ProviderConfigDiagnostic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.is_empty() {
            write!(formatter, "{}", self.message)
        } else {
            write!(formatter, "{}: {}", self.path, self.message)
        }
    }
}

#[derive(Debug, Clone)]
pub struct ParsedProvidersConfig {
    pub config: ProvidersConfig,
    pub diagnostics: Vec<ProviderConfigDiagnostic>,
}

/// Parse a provider/model catalog overlay and report keys the schema would
/// otherwise silently ignore. Unknown model/provider fields usually mean a
/// catalog migration was missed; keep serde as the schema source of truth
/// instead of hand-maintaining duplicate top-level allowlists.
pub fn parse_config_toml_with_diagnostics(
    src: &str,
) -> Result<ParsedProvidersConfig, toml::de::Error> {
    let mut diagnostics = Vec::new();
    let deserializer = toml::Deserializer::parse(src)?;
    let config = serde_ignored::deserialize(deserializer, |path| {
        diagnostics.push(unknown_field_diagnostic(path.to_string()));
    })?;
    diagnostics.extend(patch_model_unknown_field_diagnostics(src));
    Ok(ParsedProvidersConfig {
        config,
        diagnostics,
    })
}

fn unknown_field_diagnostic(path: String) -> ProviderConfigDiagnostic {
    let hint = if path.ends_with(".fast_mode") || path == "fast_mode" {
        " `fast_mode` was removed in v0.10.1; use `serving_tiers` with an explicit request knob instead."
    } else {
        ""
    };
    ProviderConfigDiagnostic {
        path,
        message: format!("unknown providers.toml field was ignored.{hint}"),
    }
}

fn patch_model_unknown_field_diagnostics(src: &str) -> Vec<ProviderConfigDiagnostic> {
    let Ok(value) = toml::from_str::<toml::Value>(src) else {
        return Vec::new();
    };
    let Some(patch_models) = value
        .get("patch")
        .and_then(|patch| patch.get("models"))
        .and_then(toml::Value::as_table)
    else {
        return Vec::new();
    };
    let mut diagnostics = Vec::new();
    for (model_id, patch) in patch_models {
        collect_model_patch_unknowns(
            &format!("patch.models.{model_id}"),
            patch,
            model_patch_schema(),
            &mut diagnostics,
        );
    }
    diagnostics
}

fn collect_model_patch_unknowns(
    path: &str,
    value: &toml::Value,
    schema: &PatchSchema,
    diagnostics: &mut Vec<ProviderConfigDiagnostic>,
) {
    let Some(table) = value.as_table() else {
        return;
    };
    for (key, child) in table {
        if schema.freeform.contains(&key.as_str()) {
            continue;
        }
        if let Some((_, nested)) = schema
            .nested
            .iter()
            .find(|(nested_key, _)| *nested_key == key.as_str())
        {
            collect_model_patch_unknowns(&format!("{path}.{key}"), child, nested, diagnostics);
            continue;
        }
        if schema.fields.contains(&key.as_str()) {
            continue;
        }
        diagnostics.push(unknown_field_diagnostic(format!("{path}.{key}")));
    }
}

struct PatchSchema {
    fields: &'static [&'static str],
    freeform: &'static [&'static str],
    nested: &'static [(&'static str, PatchSchema)],
}

fn model_patch_schema() -> &'static PatchSchema {
    static MODEL_PATCH_SCHEMA: PatchSchema = PatchSchema {
        fields: &[
            "name",
            "provider",
            "context_window",
            "logical_model",
            "equivalence_group",
            "served_variant",
            "wire_model",
            "api_dialect",
            "rate_limits",
            "performance",
            "architecture",
            "local_memory",
            "runtime_context_window",
            "stream_timeout",
            "capabilities",
            "pricing",
            "deprecated",
            "deprecation_note",
            "superseded_by",
            "serving_tiers",
            "quality_tags",
            "availability",
            "tier",
            "open_weight",
            "strengths",
            "benchmarks",
            "family",
            "lineage",
            "complementary_with",
            "avoid_as_reviewer_for",
        ],
        freeform: &["benchmarks"],
        nested: &[
            ("pricing", pricing_patch_schema()),
            ("rate_limits", rate_limits_patch_schema()),
            ("performance", performance_patch_schema()),
            ("architecture", architecture_patch_schema()),
            ("local_memory", local_memory_patch_schema()),
        ],
    };
    &MODEL_PATCH_SCHEMA
}

const fn pricing_patch_schema() -> PatchSchema {
    PatchSchema {
        fields: &[
            "input_per_mtok",
            "output_per_mtok",
            "cache_read_per_mtok",
            "cache_write_per_mtok",
        ],
        freeform: &[],
        nested: &[],
    }
}

const fn rate_limits_patch_schema() -> PatchSchema {
    PatchSchema {
        fields: &[
            "rpm",
            "rph",
            "rpd",
            "tpm",
            "tph",
            "tpd",
            "input_tpm",
            "output_tpm",
            "concurrency",
            "tier",
            "source_url",
            "last_verified",
            "notes",
        ],
        freeform: &[],
        nested: &[],
    }
}

const fn performance_patch_schema() -> PatchSchema {
    PatchSchema {
        fields: &[
            "observed_ttft_ms",
            "output_tokens_per_sec",
            "time_to_answer_s",
            "source",
            "source_url",
            "last_verified",
            "sample_size",
            "notes",
        ],
        freeform: &[],
        nested: &[],
    }
}

const fn architecture_patch_schema() -> PatchSchema {
    PatchSchema {
        fields: &[
            "parameter_count_b",
            "active_parameter_count_b",
            "moe",
            "quantization",
            "precision",
            "license",
            "tokenizer",
            "knowledge_cutoff",
            "source_url",
            "last_verified",
        ],
        freeform: &[],
        nested: &[],
    }
}

const fn local_memory_patch_schema() -> PatchSchema {
    PatchSchema {
        fields: &[
            "measured_resident_gib",
            "measured_context_window",
            "measured_cache_type",
            "base_resident_gib",
            "kv_cache_gib_per_1k_ctx",
            "cache_type_multipliers",
            "default_cache_type",
            "safety_margin_gib",
            "max_recommended_context",
            "source_url",
            "last_verified",
            "notes",
        ],
        freeform: &["cache_type_multipliers"],
        nested: &[],
    }
}
/// Returns the filesystem path of the currently-loaded providers config, if
/// any. Returns `None` when built-in defaults are active.
pub fn loaded_config_path() -> Option<std::path::PathBuf> {
    // Force lazy init so CONFIG_PATH is populated if a file was loaded.
    let _ = load_config();
    CONFIG_PATH.get().map(std::path::PathBuf::from)
}

/// Install per-run provider config overlays. The overlay uses the same shape as
/// `providers.toml`, but lives under `[llm]` in `harn.toml` and package
/// manifests. Passing `None` clears the overlay.
pub fn set_user_overrides(config: Option<ProvidersConfig>) {
    USER_OVERRIDES.with(|cell| *cell.borrow_mut() = config);
}

/// Clear per-run provider config overlays.
pub fn clear_user_overrides() {
    set_user_overrides(None);
}

/// Install the process-wide runtime catalog overlay used by
/// `provider_catalog::refresh_runtime_catalog`. Per-run user overlays still
/// merge last so project-local provider config can override hosted catalog
/// updates.
pub fn set_runtime_catalog_overlay(config: Option<ProvidersConfig>) {
    *runtime_catalog_overlay()
        .write()
        .expect("runtime catalog overlay poisoned") = config;
}

pub fn clear_runtime_catalog_overlay() {
    set_runtime_catalog_overlay(None);
}

pub(crate) fn effective_config() -> ProvidersConfig {
    let user_overrides = USER_OVERRIDES.with(|cell| cell.borrow().clone());
    effective_config_with_user_overrides(user_overrides.as_ref())
}

/// Provider config built purely from the compiled-in `EMBEDDED_PROVIDERS_TOML`
/// snapshot, ignoring every ambient layer: the developer's
/// `~/.config/harn/providers.toml`, `HARN_PROVIDERS_CONFIG`, the process
/// runtime-catalog overlay, and thread-local user overrides.
///
/// This is the hermetic source of truth for *generating* the checked-in
/// `spec/provider-catalog/*` artifacts. Artifact generation must be a pure
/// function of the source tree so a developer's personal aliases/providers
/// never leak into shipped artifacts (which then makes clean CI flag drift).
/// Runtime catalog presentation must keep using [`effective_config`] /
/// [`effective_config_with_user_overrides`], which legitimately reflect the
/// host's live configuration.
///
/// An optional explicit overlay (e.g. a `--overlay` file named on the command
/// line) is merged on top of the embedded base. Unlike the home file and env
/// layers, that overlay is a declared, reproducible input rather than ambient
/// machine state, so it is safe to honor while staying hermetic.
pub fn embedded_config(explicit_overlay: Option<&ProvidersConfig>) -> ProvidersConfig {
    let mut config = default_config();
    if let Some(overlay) = explicit_overlay {
        config.merge_from(overlay);
    }
    config
}

pub(crate) fn effective_config_with_user_overrides(
    user_overrides: Option<&ProvidersConfig>,
) -> ProvidersConfig {
    let mut merged = load_config().clone();
    if let Some(overlay) = runtime_catalog_overlay()
        .read()
        .expect("runtime catalog overlay poisoned")
        .as_ref()
    {
        merged.merge_from(overlay);
    }
    if let Some(overlay) = user_overrides {
        merged.merge_from(overlay);
    }
    merged
}

fn runtime_catalog_overlay() -> &'static RwLock<Option<ProvidersConfig>> {
    RUNTIME_CATALOG_OVERLAY.get_or_init(|| RwLock::new(None))
}

fn dirs_or_home() -> Option<String> {
    crate::user_dirs::home_dir().map(|home| home.to_string_lossy().into_owned())
}

/// Embedded copy of generated `llm/providers.toml`, built from
/// `llm/catalog_sources/**/*.toml` by `harn provider catalog build-config`.
/// Edit the fragments, not this generated snapshot or this string.
const EMBEDDED_PROVIDERS_TOML: &str = include_str!("../llm/providers.toml");

/// Parse the embedded generated `providers.toml` into the runtime
/// `ProvidersConfig`.
///
/// Hosts overlay this base via `HARN_PROVIDERS_CONFIG`,
/// `~/.config/harn/providers.toml`, `harn.toml`, package-manifest
/// `[llm]` sections, and per-run `set_user_overrides(...)`. The same
/// Serde shape applies at every layer, so there is exactly one schema to
/// keep coherent — no parallel Rust-literal catalog.
///
/// We `expect` on parse failure because the file is bundled into the
/// binary at compile time; a malformed embedded catalog is a build-time
/// invariant violation that should fail every test, not silently
/// degrade in production.
pub(crate) fn default_config() -> ProvidersConfig {
    parse_config_toml(EMBEDDED_PROVIDERS_TOML)
        .expect("embedded providers.toml must parse — invariant checked by harn-vm tests")
}

#[cfg(test)]
pub(crate) fn merge_global_config(overlay: ProvidersConfig) -> ProvidersConfig {
    let mut config = default_config();
    config.merge_from(&overlay);
    config
}
