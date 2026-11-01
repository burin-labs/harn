//! Config loading and per-run override machinery: the process-wide cached
//! config, env/home overlays, the runtime-catalog overlay, thread-local user
//! overrides, host-verified endpoint overrides, and the compiled-in default
//! catalog.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};

use super::chat_wire::{apply_chat_wire_sources, ProviderChatWireSources};
use super::*;

static CONFIG: OnceLock<Arc<ProvidersConfig>> = OnceLock::new();
static CONFIG_PATHS: OnceLock<Vec<PathBuf>> = OnceLock::new();
static CONFIG_CHAT_WIRE_SOURCES: OnceLock<ProviderChatWireSources> = OnceLock::new();
static RUNTIME_CATALOG_OVERLAY: OnceLock<RwLock<Option<ProvidersConfig>>> = OnceLock::new();

/// Lowest-precedence external provider overlay supplied by an embedding host.
/// The existing user-global config still layers on top of this file.
pub const HOST_PROVIDERS_CONFIG_ENV: &str = "HARN_HOST_PROVIDERS_CONFIG";

thread_local! {
    /// Provider config overlays for the currently-polled Harn execution.
    ///
    /// CLI bootstrap installs its package overlay here. Embedded hosts use the
    /// same slot through `orchestration::scope_llm_runtime_overrides`, which
    /// swaps it for each future poll so concurrent ACP servers cannot inherit
    /// another server's route configuration.
    static LLM_CONFIG_OVERRIDES_CONTEXT: RefCell<Option<ProvidersConfig>> = const { RefCell::new(None) };
    /// Host-verified provider endpoints for the currently-polled execution.
    ///
    /// These intentionally do not live in `ProviderDef` or `ProvidersConfig`:
    /// both are public catalog DTOs that callers construct with struct-update
    /// syntax, while a verified endpoint is session state rather than catalog
    /// data. The ambient scope carries this sidecar with the matching config.
    static LLM_RUNTIME_PROVIDER_ENDPOINTS_CONTEXT: RefCell<RuntimeProviderEndpointOverrides> = RefCell::new(RuntimeProviderEndpointOverrides::default());
}

/// Ephemeral, host-verified provider endpoints for one runtime execution.
///
/// This type is deliberately separate from TOML-backed provider definitions.
/// Embedders add an endpoint only after their own readiness check; Harn then
/// makes it authoritative for that provider for the scoped execution without
/// changing catalog persistence or the public `ProviderDef` shape.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeProviderEndpointOverrides {
    endpoints: BTreeMap<String, String>,
}

impl RuntimeProviderEndpointOverrides {
    /// Create one validated runtime endpoint override.
    pub fn single(
        provider: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Result<Self, String> {
        let mut overrides = Self::default();
        overrides.insert(provider, base_url)?;
        Ok(overrides)
    }

    /// Add or replace one verified provider endpoint.
    pub fn insert(
        &mut self,
        provider: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Result<(), String> {
        let provider = provider.into().trim().to_string();
        if provider.is_empty() {
            return Err("runtime provider endpoint requires a provider name".to_string());
        }
        let base_url = base_url.into().trim().to_string();
        if base_url.is_empty() {
            return Err(format!(
                "runtime provider endpoint for `{provider}` must not be empty"
            ));
        }
        validate_http_provider_endpoint(&base_url)
            .map_err(|error| format!("runtime provider endpoint for `{provider}`: {error}"))?;
        self.endpoints.insert(provider, base_url);
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.endpoints.is_empty()
    }

    fn endpoint_for(&self, provider: &str) -> Option<&str> {
        self.endpoints.get(provider).map(String::as_str)
    }
}

/// Load and cache the providers config. Called once at VM startup.
pub fn load_config() -> &'static ProvidersConfig {
    load_config_snapshot().as_ref()
}

fn load_config_snapshot() -> &'static Arc<ProvidersConfig> {
    CONFIG.get_or_init(|| {
        let verbose_config_logging = matches!(
            std::env::var("HARN_VERBOSE_CONFIG").ok().as_deref(),
            Some("1" | "true" | "TRUE" | "yes" | "YES")
        ) || matches!(
            std::env::var("HARN_ACP_VERBOSE").ok().as_deref(),
            Some("1" | "true" | "TRUE" | "yes" | "YES")
        );
        let host_path = std::env::var(HOST_PROVIDERS_CONFIG_ENV).ok();
        let user_path = std::env::var("HARN_PROVIDERS_CONFIG").ok();
        let home_path =
            user_providers_config_path().map(|path| path.to_string_lossy().into_owned());
        let (config, paths, sources) = load_external_config_layers_with_sources(
            default_config(),
            host_path.as_deref(),
            user_path.as_deref(),
            home_path.as_deref(),
            verbose_config_logging,
        );
        let _ = CONFIG_PATHS.set(paths);
        let _ = CONFIG_CHAT_WIRE_SOURCES.set(sources);
        Arc::new(config)
    })
}

/// Merge the host layer and then the user-global layer. An explicit user path
/// keeps the established fallback to the home file when it cannot be loaded.
#[cfg(test)]
pub(super) fn load_external_config_layers(
    config: ProvidersConfig,
    host_path: Option<&str>,
    user_path: Option<&str>,
    home_path: Option<&str>,
    verbose: bool,
) -> (ProvidersConfig, Vec<PathBuf>) {
    let (config, paths, _) =
        load_external_config_layers_with_sources(config, host_path, user_path, home_path, verbose);
    (config, paths)
}

fn load_external_config_layers_with_sources(
    mut config: ProvidersConfig,
    host_path: Option<&str>,
    user_path: Option<&str>,
    home_path: Option<&str>,
    verbose: bool,
) -> (ProvidersConfig, Vec<PathBuf>, ProviderChatWireSources) {
    let mut loaded_paths = Vec::new();
    let mut sources = ProviderChatWireSources::new();
    apply_chat_wire_sources(&mut sources, &config, ProviderConfigLayer::Embedded);
    if let Some(path) = host_path {
        merge_external_config(
            &mut config,
            path,
            verbose,
            &mut loaded_paths,
            &mut sources,
            ProviderConfigLayer::HostConfig { path: path.into() },
        );
    }
    let user_loaded = user_path.is_some_and(|path| {
        merge_external_config(
            &mut config,
            path,
            verbose,
            &mut loaded_paths,
            &mut sources,
            ProviderConfigLayer::UserConfig { path: path.into() },
        )
    });
    if !user_loaded {
        if let Some(path) = home_path {
            merge_external_config(
                &mut config,
                path,
                false,
                &mut loaded_paths,
                &mut sources,
                ProviderConfigLayer::UserConfig { path: path.into() },
            );
        }
    }
    (config, loaded_paths, sources)
}

fn merge_external_config(
    config: &mut ProvidersConfig,
    path: &str,
    verbose: bool,
    loaded_paths: &mut Vec<PathBuf>,
    sources: &mut ProviderChatWireSources,
    layer: ProviderConfigLayer,
) -> bool {
    let Some(overlay) = read_external_config(path, verbose) else {
        return false;
    };
    config.merge_from(&overlay);
    apply_chat_wire_sources(sources, &overlay, layer);
    loaded_paths.push(PathBuf::from(path));
    true
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

/// `providers.toml` in the user configuration directory. The directory's one
/// owner is [`crate::user_dirs::config_dir`]; the test environment points
/// `XDG_CONFIG_HOME` at an empty directory, so tests never read a developer's
/// provider overlay.
pub fn user_providers_config_path() -> Option<std::path::PathBuf> {
    crate::user_dirs::config_file("providers.toml")
}

/// Returns the highest-precedence loaded provider config path, if any.
/// Returns `None` when built-in defaults are active.
pub fn loaded_config_path() -> Option<std::path::PathBuf> {
    loaded_config_paths().last().cloned()
}

/// Returns every loaded external provider config path in merge order.
pub fn loaded_config_paths() -> &'static [PathBuf] {
    // Force lazy init so CONFIG_PATHS is populated even when no file loads.
    let _ = load_config();
    CONFIG_PATHS.get().map(Vec::as_slice).unwrap_or_default()
}

/// Inspect the effective chat wire contract and the layers that selected it.
/// Uses the already-loaded files, followed by the active runtime and execution
/// overlays; no diagnostic reread can disagree with the cached serving config.
pub fn provider_chat_wire_contract(provider: &str) -> Option<ProviderChatWireContract> {
    let _ = load_config();
    LLM_CONFIG_OVERRIDES_CONTEXT.with(|cell| {
        let user = cell.borrow();
        let runtime = runtime_catalog_overlay()
            .read()
            .expect("runtime catalog overlay poisoned");
        let config =
            effective_config_snapshot(load_config_snapshot(), runtime.as_ref(), user.as_ref());
        let definition = config.providers.get(provider)?;
        let mut sources = CONFIG_CHAT_WIRE_SOURCES
            .get()
            .and_then(|sources| sources.get(provider))
            .cloned()
            .unwrap_or_default();
        for (overlay, layer) in [
            (runtime.as_ref(), ProviderConfigLayer::RuntimeCatalog),
            (user.as_ref(), ProviderConfigLayer::ExecutionOverride),
        ] {
            if let Some(definition) = overlay.and_then(|config| config.providers.get(provider)) {
                sources.apply(definition, &layer);
            }
        }
        Some(sources.contract(definition))
    })
}

/// Install per-run provider config overlays. The overlay uses the same shape as
/// `providers.toml`, but lives under `[llm]` in `harn.toml` and package
/// manifests. Passing `None` clears the overlay.
pub fn set_user_overrides(config: Option<ProvidersConfig>) {
    LLM_CONFIG_OVERRIDES_CONTEXT.with(|cell| *cell.borrow_mut() = config);
}

/// Collect explicit execution restrictions without letting one row hide another.
pub(crate) fn execution_training_restrictions(provider: &str, model: &str) -> Vec<TrainingDefault> {
    let route = super::model_catalog_id_for_route(provider, model);
    let endpoint =
        super::provider_config(provider).map(|definition| super::resolve_base_url(&definition));
    LLM_CONFIG_OVERRIDES_CONTEXT.with(|cell| {
        let config = cell.borrow();
        let Some(config) = config.as_ref() else {
            return Vec::new();
        };
        let mut restrictions = Vec::new();
        if let Some(definition) = config.providers.get(provider) {
            restrictions.extend(
                definition
                    .data_controls
                    .iter()
                    .map(|controls| controls.training_default),
            );
            restrictions.extend(
                definition
                    .regions
                    .values()
                    .filter(|region| Some(region.base_url.as_str()) == endpoint.as_deref())
                    .filter_map(|region| {
                        region
                            .data_controls
                            .as_ref()
                            .map(|controls| controls.training_default)
                    }),
            );
        }
        for id in [Some(model), route.as_deref()].into_iter().flatten() {
            restrictions.extend(
                config
                    .models
                    .get(id)
                    .and_then(|row| row.data_controls.as_ref())
                    .map(|controls| controls.training_default),
            );
        }
        restrictions
    })
}

/// Swap the per-execution provider overrides and return the previous value.
///
/// `AmbientExecutionScope` owns the poll-scoped use of this primitive. Normal
/// callers should use [`set_user_overrides`] or
/// `orchestration::scope_llm_runtime_overrides` instead of moving this context
/// themselves.
pub(crate) fn swap_user_overrides(next: Option<ProvidersConfig>) -> Option<ProvidersConfig> {
    LLM_CONFIG_OVERRIDES_CONTEXT.with(|cell| std::mem::replace(&mut *cell.borrow_mut(), next))
}

/// Swap host-verified provider endpoints for the current ambient execution.
/// `AmbientExecutionScope` owns asynchronous use of this primitive.
pub(crate) fn swap_runtime_provider_endpoint_overrides(
    next: RuntimeProviderEndpointOverrides,
) -> RuntimeProviderEndpointOverrides {
    LLM_RUNTIME_PROVIDER_ENDPOINTS_CONTEXT
        .with(|cell| std::mem::replace(&mut *cell.borrow_mut(), next))
}

/// Install host-verified provider endpoints for the current thread.
///
/// Normal async embedders should use
/// [`crate::orchestration::scope_llm_runtime_overrides_with_provider_endpoints`]
/// so concurrent executions cannot inherit one another's routes.
pub fn set_runtime_provider_endpoint_overrides(overrides: RuntimeProviderEndpointOverrides) {
    let _ = swap_runtime_provider_endpoint_overrides(overrides);
}

/// Clear host-verified provider endpoints for the current thread.
pub fn clear_runtime_provider_endpoint_overrides() {
    set_runtime_provider_endpoint_overrides(RuntimeProviderEndpointOverrides::default());
}

pub(crate) fn runtime_provider_endpoint(provider: &str) -> Option<String> {
    LLM_RUNTIME_PROVIDER_ENDPOINTS_CONTEXT
        .with(|cell| cell.borrow().endpoint_for(provider).map(str::to_string))
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

/// Return the effective catalog as an immutable snapshot.
///
/// The overwhelmingly common no-overlay path shares the process-wide catalog;
/// runtime and per-execution overlays still produce an isolated merged value.
pub(crate) fn effective_config() -> Arc<ProvidersConfig> {
    LLM_CONFIG_OVERRIDES_CONTEXT.with(|cell| {
        let user_overrides = cell.borrow();
        let runtime_overlay = runtime_catalog_overlay()
            .read()
            .expect("runtime catalog overlay poisoned");
        effective_config_snapshot(
            load_config_snapshot(),
            runtime_overlay.as_ref(),
            user_overrides.as_ref(),
        )
    })
}

/// Host and verified catalog facts exclude package-scoped routing overrides.
pub(crate) fn trusted_policy_config() -> Arc<ProvidersConfig> {
    let runtime_overlay = runtime_catalog_overlay()
        .read()
        .expect("runtime catalog overlay poisoned");
    effective_config_snapshot(load_config_snapshot(), runtime_overlay.as_ref(), None)
}

fn effective_config_snapshot(
    base: &Arc<ProvidersConfig>,
    runtime_overlay: Option<&ProvidersConfig>,
    user_overrides: Option<&ProvidersConfig>,
) -> Arc<ProvidersConfig> {
    if runtime_overlay.is_none() && user_overrides.is_none() {
        Arc::clone(base)
    } else {
        Arc::new(merge_config_overlays(base, runtime_overlay, user_overrides))
    }
}

fn merge_config_overlays(
    base: &ProvidersConfig,
    runtime_overlay: Option<&ProvidersConfig>,
    user_overrides: Option<&ProvidersConfig>,
) -> ProvidersConfig {
    let mut merged = base.clone();
    if let Some(overlay) = runtime_overlay {
        merged.merge_from(overlay);
    }
    if let Some(overlay) = user_overrides {
        merged.merge_from(overlay);
    }
    merged
}

pub(crate) fn effective_config_with_user_overrides(
    user_overrides: Option<&ProvidersConfig>,
) -> ProvidersConfig {
    let runtime_overlay = runtime_catalog_overlay()
        .read()
        .expect("runtime catalog overlay poisoned");
    merge_config_overlays(load_config(), runtime_overlay.as_ref(), user_overrides)
}

fn runtime_catalog_overlay() -> &'static RwLock<Option<ProvidersConfig>> {
    RUNTIME_CATALOG_OVERLAY.get_or_init(|| RwLock::new(None))
}

/// Parse the embedded generated `providers.toml` into the runtime
/// `ProvidersConfig`.
///
/// Hosts overlay this base via `HARN_HOST_PROVIDERS_CONFIG`; users then layer
/// `HARN_PROVIDERS_CONFIG`, `~/.config/harn/providers.toml`, `harn.toml`,
/// package-manifest `[llm]` sections, and per-run `set_user_overrides(...)`.
/// The same Serde shape applies at every layer, so there is exactly one schema
/// to keep coherent — no parallel Rust-literal catalog.
///
/// We `expect` on parse failure because the file is bundled into the
/// binary at compile time; a malformed embedded catalog is a build-time
/// invariant violation that should fail every test, not silently
/// degrade in production.
pub(crate) fn default_config() -> ProvidersConfig {
    harn_provider_catalog::config::packaged_config().clone()
}

#[cfg(test)]
pub(crate) fn merge_global_config(overlay: ProvidersConfig) -> ProvidersConfig {
    let mut config = default_config();
    config.merge_from(&overlay);
    config
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;

    #[test]
    fn endpoint_only_user_layer_resets_host_adapter_and_its_source() {
        let directory = tempfile::tempdir().unwrap();
        let host = directory.path().join("host.toml");
        let home = directory.path().join("home.toml");
        std::fs::write(&host, "[providers.ollama]\nchat_endpoint = '/v1/chat/completions'\nchat_api_adapter = 'ollama_openai_compat'\n").unwrap();
        std::fs::write(&home, "[providers.ollama]\nchat_endpoint = '/api/chat'\n").unwrap();
        let (config, paths, sources) = load_external_config_layers_with_sources(
            default_config(),
            host.to_str(),
            None,
            home.to_str(),
            false,
        );
        assert_eq!(paths, vec![host, home.clone()]);
        let contract = sources["ollama"]
            .clone()
            .contract(&config.providers["ollama"]);
        assert_eq!(contract.chat_endpoint, "/api/chat");
        assert_eq!(contract.chat_api_adapter, ChatApiAdapter::ModelDefault);
        let source = Some(ProviderConfigLayer::UserConfig {
            path: home.to_string_lossy().into_owned(),
        });
        assert_eq!(contract.sources.chat_endpoint, source);
        assert_eq!(contract.sources.chat_api_adapter, source);

        // An unrelated later layer must preserve the measured wire contract.
        std::fs::write(
            &home,
            "[providers.ollama]\nbase_url = 'http://localhost:11435'\n",
        )
        .unwrap();
        let (config, _, sources) = load_external_config_layers_with_sources(
            default_config(),
            paths[0].to_str(),
            None,
            home.to_str(),
            false,
        );
        assert_eq!(
            config.providers["ollama"].chat_api_adapter,
            Some(ChatApiAdapter::OllamaOpenAiCompat)
        );
        assert!(matches!(
            sources["ollama"].chat_api_adapter,
            Some(ProviderConfigLayer::HostConfig { .. })
        ));
    }

    fn config(default_provider: &str, qc_defaults: &[(&str, &str)]) -> ProvidersConfig {
        ProvidersConfig {
            default_provider: Some(default_provider.to_string()),
            qc_defaults: qc_defaults
                .iter()
                .map(|(provider, model)| ((*provider).to_string(), (*model).to_string()))
                .collect(),
            ..ProvidersConfig::default()
        }
    }

    #[test]
    fn effective_snapshot_reuses_base_without_overlays() {
        let base = Arc::new(config("base", &[("base", "base/model")]));

        let first = effective_config_snapshot(&base, None, None);
        let second = effective_config_snapshot(&base, None, None);

        assert!(Arc::ptr_eq(&base, &first));
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn effective_snapshot_isolates_and_exactly_merges_overlays() {
        let base = Arc::new(config(
            "base",
            &[("base", "base/model"), ("shared", "base/shared")],
        ));
        let runtime = config(
            "runtime",
            &[("runtime", "runtime/model"), ("shared", "runtime/shared")],
        );
        let user = config("user", &[("user", "user/model"), ("shared", "user/shared")]);

        let merged = effective_config_snapshot(&base, Some(&runtime), Some(&user));

        assert!(!Arc::ptr_eq(&base, &merged));
        assert_eq!(
            merged.as_ref(),
            &config(
                "user",
                &[
                    ("base", "base/model"),
                    ("runtime", "runtime/model"),
                    ("shared", "user/shared"),
                    ("user", "user/model"),
                ],
            )
        );
        assert_eq!(
            base.as_ref(),
            &config("base", &[("base", "base/model"), ("shared", "base/shared")])
        );
    }
}
