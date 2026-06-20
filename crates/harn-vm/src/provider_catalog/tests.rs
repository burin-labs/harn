use super::*;

use base64::Engine as _;
use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Mutex, MutexGuard};

static RUNTIME_REFRESH_TEST_LOCK: Mutex<()> = Mutex::new(());

struct OverrideGuard;

impl Drop for OverrideGuard {
    fn drop(&mut self) {
        llm_config::clear_user_overrides();
    }
}

struct RuntimeCatalogGuard {
    _lock: MutexGuard<'static, ()>,
    _runtime_paths_env_lock: MutexGuard<'static, ()>,
    state_dir: tempfile::TempDir,
    previous_state_dir: Option<String>,
    previous_allow_unsigned: Option<String>,
    previous_disable_refresh: Option<String>,
    previous_trusted_keys: Option<String>,
}

impl RuntimeCatalogGuard {
    fn new() -> Self {
        let lock = RUNTIME_REFRESH_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let runtime_paths_env_lock = crate::runtime_paths::test_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state_dir = tempfile::tempdir().expect("temp state dir");
        let previous_state_dir = std::env::var(crate::runtime_paths::HARN_STATE_DIR_ENV).ok();
        let previous_allow_unsigned = std::env::var(HARN_PROVIDER_CATALOG_ALLOW_UNSIGNED_ENV).ok();
        let previous_disable_refresh = std::env::var(HARN_DISABLE_CATALOG_REFRESH_ENV).ok();
        let previous_trusted_keys = std::env::var(HARN_PROVIDER_CATALOG_TRUSTED_KEYS_ENV).ok();
        unsafe {
            std::env::set_var(crate::runtime_paths::HARN_STATE_DIR_ENV, state_dir.path());
            std::env::remove_var(HARN_PROVIDER_CATALOG_ALLOW_UNSIGNED_ENV);
            std::env::remove_var(HARN_DISABLE_CATALOG_REFRESH_ENV);
            std::env::remove_var(HARN_PROVIDER_CATALOG_TRUSTED_KEYS_ENV);
        }
        llm_config::clear_runtime_catalog_overlay();
        Self {
            _lock: lock,
            _runtime_paths_env_lock: runtime_paths_env_lock,
            state_dir,
            previous_state_dir,
            previous_allow_unsigned,
            previous_disable_refresh,
            previous_trusted_keys,
        }
    }
}

impl Drop for RuntimeCatalogGuard {
    fn drop(&mut self) {
        llm_config::clear_runtime_catalog_overlay();
        match self.previous_state_dir.as_deref() {
            Some(value) => unsafe {
                std::env::set_var(crate::runtime_paths::HARN_STATE_DIR_ENV, value);
            },
            None => unsafe { std::env::remove_var(crate::runtime_paths::HARN_STATE_DIR_ENV) },
        }
        restore_env_var(
            HARN_PROVIDER_CATALOG_ALLOW_UNSIGNED_ENV,
            self.previous_allow_unsigned.as_deref(),
        );
        restore_env_var(
            HARN_DISABLE_CATALOG_REFRESH_ENV,
            self.previous_disable_refresh.as_deref(),
        );
        restore_env_var(
            HARN_PROVIDER_CATALOG_TRUSTED_KEYS_ENV,
            self.previous_trusted_keys.as_deref(),
        );
    }
}

fn restore_env_var(name: &str, value: Option<&str>) {
    match value {
        Some(value) => unsafe { std::env::set_var(name, value) },
        None => unsafe { std::env::remove_var(name) },
    }
}

fn install_overlay(toml_src: &str) -> OverrideGuard {
    let overlay = llm_config::parse_config_toml(toml_src).expect("overlay parses");
    llm_config::set_user_overrides(Some(overlay));
    OverrideGuard
}

fn remote_catalog_with_extra_model() -> ProviderCatalogArtifact {
    let mut remote = artifact();
    let mut provider = remote.providers[0].clone();
    provider.id = "refreshco".to_string();
    provider.display_name = "Refresh Co".to_string();
    provider.endpoint.base_url = "https://refresh.example/v1".to_string();
    provider.auth.style = "none".to_string();
    provider.auth.required = false;
    provider.auth.env.clear();
    remote.providers.push(provider);

    let mut model = remote.models[0].clone();
    model.id = "refreshco/new-model".to_string();
    model.name = "Refresh Co New Model".to_string();
    model.provider = "refreshco".to_string();
    model.aliases = vec!["refresh-new".to_string()];
    model.context_window = 123_456;
    model.deprecation.status = DeprecationStatus::Active;
    model.deprecation.note = None;
    model.deprecation.superseded_by = None;
    remote.models.push(model);

    remote.aliases.push(CatalogAlias {
        name: "refresh-new".to_string(),
        model_id: "refreshco/new-model".to_string(),
        provider: "refreshco".to_string(),
        tool_format: Some("text".to_string()),
        tool_calling: None,
    });
    remote
}

fn spawn_catalog_stub(body: String) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind catalog stub");
    let url = format!("http://{}/catalog.json", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept catalog request");
        let mut request = [0; 1024];
        let _ = stream.read(&mut request);
        let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\netag: \"fixture-v1\"\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
        stream
            .write_all(response.as_bytes())
            .expect("write catalog response");
    });
    (url, handle)
}

#[test]
fn generated_catalog_validates() {
    llm_config::clear_user_overrides();
    let report = validate_current();
    assert!(
        report.errors.is_empty(),
        "catalog validation errors: {:?}",
        report.errors
    );
}

#[test]
fn generated_catalog_exports_provider_healthcheck_metadata() {
    llm_config::clear_user_overrides();
    let catalog = artifact();

    let anthropic = catalog
        .providers
        .iter()
        .find(|provider| provider.id == "anthropic")
        .expect("anthropic provider is exported");
    assert_eq!(
        anthropic
            .extra_headers
            .get("anthropic-version")
            .map(String::as_str),
        Some("2023-06-01")
    );
    let anthropic_healthcheck = anthropic
        .healthcheck
        .as_ref()
        .expect("anthropic healthcheck is exported");
    assert_eq!(anthropic_healthcheck.method, "POST");
    assert_eq!(
        anthropic_healthcheck.path.as_deref(),
        Some("/messages/count_tokens")
    );
    assert!(
        anthropic_healthcheck
            .body
            .as_deref()
            .is_some_and(|body| body.contains("claude-sonnet-4-6")),
        "anthropic healthcheck body should include a token-count model"
    );

    let openrouter = catalog
        .providers
        .iter()
        .find(|provider| provider.id == "openrouter")
        .expect("openrouter provider is exported");
    assert_eq!(
        openrouter
            .healthcheck
            .as_ref()
            .and_then(|healthcheck| healthcheck.path.as_deref()),
        Some("/auth/key")
    );
}

#[test]
fn catalog_roundtrips_provider_healthcheck_metadata_into_config() {
    llm_config::clear_user_overrides();
    let catalog = artifact();
    let config = config_from_artifact(&catalog);
    let anthropic = config
        .providers
        .get("anthropic")
        .expect("anthropic provider roundtrips");

    assert_eq!(
        anthropic
            .extra_headers
            .get("anthropic-version")
            .map(String::as_str),
        Some("2023-06-01")
    );
    assert_eq!(
        anthropic
            .healthcheck
            .as_ref()
            .and_then(|healthcheck| healthcheck.path.as_deref()),
        Some("/messages/count_tokens")
    );
}

#[tokio::test]
async fn runtime_refresh_installs_valid_remote_catalog_overlay() {
    let guard = RuntimeCatalogGuard::new();
    let remote = remote_catalog_with_extra_model();
    let body = serde_json::to_string(&remote).expect("remote catalog serializes");
    let (url, server) = spawn_catalog_stub(body);

    let report = refresh_runtime_catalog(CatalogRefreshOptions {
        url: Some(url),
        force: true,
    })
    .await;
    server.join().expect("catalog server exits");

    assert_eq!(report.status, "refreshed");
    assert!(report.refreshed);
    assert_eq!(report.etag.as_deref(), Some("\"fixture-v1\""));
    assert!(guard
        .state_dir
        .path()
        .join("cache/provider-catalog/catalog.json")
        .is_file());

    let refreshed = llm_config::model_catalog_entry("refreshco/new-model")
        .expect("remote model installed into runtime catalog");
    assert_eq!(refreshed.name, "Refresh Co New Model");
    assert_eq!(refreshed.context_window, 123_456);
    assert!(llm_config::known_model_names()
        .iter()
        .any(|name| name == "refresh-new"));
}

#[tokio::test]
async fn runtime_refresh_rejects_malformed_remote_without_emptying_catalog() {
    let _guard = RuntimeCatalogGuard::new();
    let baseline_count = llm_config::model_catalog_entries().len();
    let (url, server) = spawn_catalog_stub(r#"{"schema_version":2,"models":[]}"#.to_string());

    let report = refresh_runtime_catalog(CatalogRefreshOptions {
        url: Some(url),
        force: true,
    })
    .await;
    server.join().expect("catalog server exits");

    assert_eq!(report.status, "fallback");
    assert!(report.warning.as_deref().is_some_and(|warning| {
        warning.contains("catalog JSON does not match")
            || warning.contains("catalog has no providers")
            || warning.contains("unsigned")
    }));
    assert_eq!(llm_config::model_catalog_entries().len(), baseline_count);
}

#[test]
fn signed_catalog_envelope_accepts_trusted_key() {
    let _guard = RuntimeCatalogGuard::new();
    let catalog = remote_catalog_with_extra_model();
    let signing_key = SigningKey::from_bytes(&[42; 32]);
    let canonical = serde_json::to_vec(&catalog).expect("catalog canonicalizes");
    let signature = signing_key.sign(&canonical);
    let public_key =
        base64::engine::general_purpose::STANDARD.encode(signing_key.verifying_key().to_bytes());
    unsafe {
        std::env::set_var(
            HARN_PROVIDER_CATALOG_TRUSTED_KEYS_ENV,
            format!("test={public_key}"),
        );
    }
    let document = json!({
        "ttlMS": 1_234,
        "catalog": catalog,
        "signature": {
            "algorithm": "ed25519",
            "key_id": "test",
            "signature": base64::engine::general_purpose::STANDARD.encode(signature.to_bytes()),
        },
    });

    let decoded =
        remote::decode_and_validate_document(&document.to_string(), false).expect("signed catalog");

    assert_eq!(decoded.ttl_ms, 1_234);
    assert!(decoded
        .artifact
        .models
        .iter()
        .any(|model| model.id == "refreshco/new-model"));
}

#[test]
fn generated_catalog_derives_quality_tags_from_routes() {
    let catalog = artifact();
    let frontier = catalog
        .models
        .iter()
        .find(|model| model.aliases.iter().any(|alias| alias == "frontier"))
        .expect("frontier alias target is exported");
    assert!(frontier.quality_tags.iter().any(|tag| tag == "frontier"));

    let local = catalog
        .models
        .iter()
        .find(|model| model.aliases.iter().any(|alias| alias == "local-gemma4"))
        .expect("local alias target is exported");
    assert!(local.quality_tags.iter().any(|tag| tag == "local"));
}

#[test]
fn validation_rejects_missing_required_metadata() {
    let mut catalog = artifact();
    catalog.providers[0].display_name.clear();
    let report = validate_artifact(&catalog);
    assert!(
        report
            .errors
            .iter()
            .any(|message| message.contains("display_name cannot be empty")),
        "expected provider metadata validation error, got {:?}",
        report.errors
    );
}

#[test]
fn validation_rejects_duplicate_and_dangling_aliases() {
    let mut duplicated = artifact();
    duplicated.aliases.push(duplicated.aliases[0].clone());
    let duplicate_report = validate_artifact(&duplicated);
    assert!(
        duplicate_report
            .errors
            .iter()
            .any(|message| message.contains("duplicate alias name")),
        "expected duplicate alias validation error, got {:?}",
        duplicate_report.errors
    );

    let mut dangling = artifact();
    dangling.aliases[0].model_id = "missing-model".to_string();
    let dangling_report = validate_artifact(&dangling);
    assert!(
        dangling_report
            .errors
            .iter()
            .any(|message| message.contains("without a catalog row")),
        "expected dangling alias validation error, got {:?}",
        dangling_report.errors
    );
}

#[test]
fn validation_rejects_alias_tool_format_not_a_known_format() {
    // A typo / wrong value in an alias's tool_format pin must be caught at
    // catalog-build time, not silently degraded at call time. The known set is
    // native, text, and json (the fenced-JSON text-channel format).
    let mut catalog = artifact();
    catalog.aliases[0].tool_format = Some("nativ".to_string());
    let report = validate_artifact(&catalog);
    assert!(
        report
            .errors
            .iter()
            .any(|message| message.contains("must be \"native\", \"text\", or \"json\"")),
        "expected invalid alias tool_format error, got {:?}",
        report.errors
    );
}

#[test]
fn validation_accepts_alias_pinning_json_on_text_capable_model() {
    // `json` (fenced-JSON) is a first-class text-channel format. An alias may
    // pin it, and it validates against the model's text tool support — not a
    // typo/wrong value, and not gated on native support.
    let mut catalog = artifact();
    let alias_target = (
        catalog.aliases[0].provider.clone(),
        catalog.aliases[0].model_id.clone(),
    );
    let model = catalog
        .models
        .iter_mut()
        .find(|m| m.provider == alias_target.0 && m.id == alias_target.1)
        .expect("alias target model present in catalog");
    model.tool_support.native = false;
    model.tool_support.text = true;
    catalog.aliases[0].tool_format = Some("json".to_string());
    let report = validate_artifact(&catalog);
    assert!(
        !report.errors.iter().any(|message| message
            .contains("must be \"native\", \"text\", or \"json\"")
            || message.contains("does not support")),
        "json pinned on a text-capable model should validate, got {:?}",
        report.errors
    );

    // Symmetric: pinning `json` on a model with no text support is a hard error
    // (json rides the text channel, so it requires text tool support).
    let mut catalog = artifact();
    let alias_target = (
        catalog.aliases[0].provider.clone(),
        catalog.aliases[0].model_id.clone(),
    );
    let model = catalog
        .models
        .iter_mut()
        .find(|m| m.provider == alias_target.0 && m.id == alias_target.1)
        .expect("alias target model present in catalog");
    model.tool_support.native = true;
    model.tool_support.text = false;
    catalog.aliases[0].tool_format = Some("json".to_string());
    let report = validate_artifact(&catalog);
    assert!(
        report
            .errors
            .iter()
            .any(|message| message.contains("does not support text tool calling")),
        "expected json-on-text-unsupported alias error, got {:?}",
        report.errors
    );
}

#[test]
fn validation_rejects_alias_pinning_unservable_tool_format() {
    // An alias may only pin a format the target model actually serves. Pin
    // "native" on a text-only model -> hard error (this is the exact footgun
    // that the shipped ollama-devstral-small-2-native alias had).
    let mut catalog = artifact();
    let alias_target = (
        catalog.aliases[0].provider.clone(),
        catalog.aliases[0].model_id.clone(),
    );
    let model = catalog
        .models
        .iter_mut()
        .find(|m| m.provider == alias_target.0 && m.id == alias_target.1)
        .expect("alias target model present in catalog");
    model.tool_support.native = false;
    model.tool_support.text = true;
    catalog.aliases[0].tool_format = Some("native".to_string());
    let report = validate_artifact(&catalog);
    assert!(
        report
            .errors
            .iter()
            .any(|message| message.contains("does not support native tool calling")),
        "expected unservable-native alias error, got {:?}",
        report.errors
    );

    // Symmetric: pin "text" on a native-only model -> hard error.
    let mut catalog = artifact();
    let alias_target = (
        catalog.aliases[0].provider.clone(),
        catalog.aliases[0].model_id.clone(),
    );
    let model = catalog
        .models
        .iter_mut()
        .find(|m| m.provider == alias_target.0 && m.id == alias_target.1)
        .expect("alias target model present in catalog");
    model.tool_support.native = true;
    model.tool_support.text = false;
    catalog.aliases[0].tool_format = Some("text".to_string());
    let report = validate_artifact(&catalog);
    assert!(
        report
            .errors
            .iter()
            .any(|message| message.contains("does not support text tool calling")),
        "expected unservable-text alias error, got {:?}",
        report.errors
    );
}

#[test]
fn validation_accepts_alias_pinning_servable_tool_format() {
    // The happy path: an alias pinning a format the model serves validates.
    let mut catalog = artifact();
    let alias_target = (
        catalog.aliases[0].provider.clone(),
        catalog.aliases[0].model_id.clone(),
    );
    let model = catalog
        .models
        .iter_mut()
        .find(|m| m.provider == alias_target.0 && m.id == alias_target.1)
        .expect("alias target model present in catalog");
    model.tool_support.native = true;
    model.tool_support.text = true;
    catalog.aliases[0].tool_format = Some("native".to_string());
    let report = validate_artifact(&catalog);
    assert!(
        !report
            .errors
            .iter()
            .any(|message| message.contains("does not support")
                || message.contains("must be \"native\", \"text\", or \"json\"")),
        "servable alias tool_format should validate, got {:?}",
        report.errors
    );
}

#[test]
fn overlay_merge_surfaces_private_model() {
    let _guard = install_overlay(
        r#"
[providers.private]
display_name = "Private"
base_url = "http://127.0.0.1:9000"
auth_style = "none"
chat_endpoint = "/v1/chat/completions"

[aliases]
private-fast = { id = "private/fast", provider = "private" }

[models."private/fast"]
name = "Private Fast"
provider = "private"
context_window = 8192
quality_tags = ["experiment"]
"#,
    );
    let catalog = artifact();
    assert!(catalog.providers.iter().any(|p| p.id == "private"));
    let model = catalog
        .models
        .iter()
        .find(|model| model.id == "private/fast")
        .expect("private model is exported");
    assert_eq!(model.aliases, vec!["private-fast"]);
    assert_eq!(model.quality_tags, vec!["experiment"]);
}

#[test]
fn cataloged_models_default_to_serverless_availability() {
    llm_config::clear_user_overrides();
    let catalog = artifact();
    let qwen_dedicated = catalog
        .models
        .iter()
        .find(|model| model.id == "Qwen/Qwen3-Coder-Next-FP8")
        .expect("Together dedicated route is exported");
    assert_eq!(
        qwen_dedicated.availability,
        ModelAvailabilityStatus::Dedicated
    );

    let bundled_serverless = catalog
        .models
        .iter()
        .find(|model| model.id == "qwen/qwen3-coder")
        .expect("OpenRouter Qwen3 Coder is exported");
    assert_eq!(
        bundled_serverless.availability,
        ModelAvailabilityStatus::Serverless
    );
}

#[test]
fn tier_alias_targeting_dedicated_model_emits_warning() {
    let _guard = install_overlay(
        r#"
[providers.together_test]
display_name = "Together (test)"
base_url = "https://api.together.xyz/v1"
auth_style = "bearer"
auth_env = "TOGETHER_AI_API_KEY"
chat_endpoint = "/chat/completions"

[aliases.frontier]
id = "Qwen/Test-Dedicated-Only"
provider = "together_test"

[models."Qwen/Test-Dedicated-Only"]
name = "Qwen Dedicated Only"
provider = "together_test"
context_window = 8192
availability = "dedicated"
"#,
    );
    let report = validate_current();
    assert!(
        report.warnings.iter().any(|message| {
            message.contains("tier alias frontier") && message.contains("dedicated-only model")
        }),
        "expected dedicated-alias warning, got {:?}",
        report.warnings
    );
}

#[test]
fn overlay_parses_availability_strings() {
    let _guard = install_overlay(
        r#"
[providers.experiment_co]
display_name = "Experiment Co"
base_url = "https://example.test/v1"
auth_style = "bearer"
auth_env = "EXPERIMENT_API_KEY"
chat_endpoint = "/chat/completions"

[models."exp/discovered"]
name = "Discovered Route"
provider = "experiment_co"
context_window = 4096
availability = "unknown"
"#,
    );
    let catalog = artifact();
    let model = catalog
        .models
        .iter()
        .find(|model| model.id == "exp/discovered")
        .expect("overlay model is exported");
    assert_eq!(model.availability, ModelAvailabilityStatus::Unknown);
}

#[test]
fn catalog_exports_family_and_lineage_for_hosted_wrappers() {
    let catalog = artifact();
    let hosted_claude = catalog
        .models
        .iter()
        .find(|model| model.id == "anthropic/claude-sonnet-4-6")
        .expect("OpenRouter Claude wrapper is exported");
    assert_eq!(hosted_claude.provider, "openrouter");
    assert_eq!(hosted_claude.family, "anthropic-claude");
    assert_eq!(hosted_claude.lineage, "claude-sonnet-opus");

    let direct_gemini = catalog
        .models
        .iter()
        .find(|model| model.id == "gemini-2.5-flash")
        .expect("Gemini Flash is exported");
    assert_eq!(direct_gemini.family, "google-gemini");
    assert_eq!(direct_gemini.lineage, "gemini-flash");
}

#[test]
fn validation_rejects_malformed_family_metadata() {
    let mut catalog = artifact();
    catalog.models[0].family = "Not Normalized".to_string();
    catalog.models[0].lineage.clear();
    let report = validate_artifact(&catalog);
    assert!(
        report
            .errors
            .iter()
            .any(|message| message.contains("family")),
        "expected family validation error, got {:?}",
        report.errors
    );
    assert!(
        report
            .errors
            .iter()
            .any(|message| message.contains("lineage")),
        "expected lineage validation error, got {:?}",
        report.errors
    );
}

#[test]
fn deprecated_models_require_notes() {
    let _guard = install_overlay(
        r#"
[models."old-model"]
name = "Old Model"
provider = "openai"
context_window = 4096
deprecated = true
"#,
    );
    let report = validate_current();
    assert!(
        report
            .errors
            .iter()
            .any(|message| message.contains("deprecated model old-model")),
        "expected deprecation validation error, got {:?}",
        report.errors
    );
}

#[test]
fn generated_schema_accepts_generated_artifact_shape() {
    let schema = schema_value();
    assert_eq!(schema["$id"], PROVIDER_CATALOG_SCHEMA_ID);
    assert_eq!(
        schema["$defs"]["tool_support"]["properties"]["empirical_parity"]["$ref"],
        "#/$defs/tool_empirical_parity"
    );
    assert!(schema["$defs"]["model"]["required"]
        .as_array()
        .is_some_and(|required| required.iter().any(|field| field == "family")));
    assert!(schema["$defs"]["model"]["required"]
        .as_array()
        .is_some_and(|required| required.iter().any(|field| field == "lineage")));
    let artifact_value = serde_json::to_value(artifact()).expect("artifact serializes");
    assert_eq!(
        artifact_value["schema_version"],
        PROVIDER_CATALOG_SCHEMA_VERSION
    );
    assert!(artifact_value["providers"]
        .as_array()
        .is_some_and(|v| !v.is_empty()));
    assert!(artifact_value["models"]
        .as_array()
        .is_some_and(|v| !v.is_empty()));
    assert!(artifact_value["models"][0]["family"].is_string());
    assert!(artifact_value["models"][0]["lineage"].is_string());
}

#[test]
fn downstream_bindings_include_empirical_tool_parity_shape() {
    let typescript = typescript_binding().expect("typescript binding renders");
    assert!(typescript.contains("empirical_parity?: HarnToolEmpiricalParity"));
    assert!(typescript.contains("export interface HarnToolEmpiricalParity"));

    let swift = swift_binding().expect("swift binding renders");
    assert!(swift.contains("public let empiricalParity: HarnToolEmpiricalParity?"));
    assert!(swift.contains("public struct HarnToolEmpiricalParity"));
}

#[test]
fn fast_mode_and_supersession_surface_in_contract() {
    let schema = schema_value();
    assert_eq!(
        schema["$defs"]["model"]["properties"]["fast_mode"]["$ref"],
        "#/$defs/fast_mode"
    );
    assert_eq!(
        schema["$defs"]["fast_mode"]["properties"]["pricing"]["$ref"],
        "#/$defs/pricing"
    );
    assert!(schema["$defs"]["deprecation"]["properties"]["superseded_by"].is_object());

    let typescript = typescript_binding().expect("typescript binding renders");
    assert!(typescript.contains("export interface HarnModelFastMode"));
    assert!(typescript.contains("fast_mode?: HarnModelFastMode"));
    assert!(typescript.contains("superseded_by?: string"));
    assert!(typescript.contains("family: string"));
    assert!(typescript.contains("lineage: string"));

    let swift = swift_binding().expect("swift binding renders");
    assert!(swift.contains("public struct HarnModelFastMode"));
    assert!(swift.contains("public let fastMode: HarnModelFastMode?"));
    assert!(swift.contains("case supersededBy = \"superseded_by\""));
    assert!(swift.contains("public let family: String"));
    assert!(swift.contains("public let lineage: String"));
}

#[test]
fn dangling_superseded_by_and_unknown_fast_status_warn() {
    let _guard = install_overlay(
        r#"
[providers.warn_co]
display_name = "Warn Co"
base_url = "https://example.test/v1"
auth_style = "bearer"
auth_env = "WARN_API_KEY"
chat_endpoint = "/chat/completions"

[models."warn/old"]
name = "Warn Old"
provider = "warn_co"
context_window = 4096
deprecated = true
deprecation_note = "Retiring soon."
superseded_by = "warn/does-not-exist"
fast_mode = { param = "speed", value = "fast", status = "turbo", pricing = { input_per_mtok = 1.0, output_per_mtok = 2.0 } }
"#,
    );
    let report = validate_current();
    assert!(
        report
            .warnings
            .iter()
            .any(|message| message.contains("superseded_by warn/does-not-exist")),
        "expected dangling superseded_by warning, got {:?}",
        report.warnings
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|message| message.contains("fast_mode.status") && message.contains("turbo")),
        "expected fast_mode.status warning, got {:?}",
        report.warnings
    );
}

/// A providers.toml overlay declaring a provider/alias/model that a developer
/// might carry in their personal `~/.config/harn/providers.toml`. Generation
/// must never bake any of this into the shipped catalog artifacts.
const POLLUTING_OVERLAY: &str = r#"
[providers.leakco]
display_name = "Leak Co"
base_url = "https://leak.example/v1"
auth_style = "bearer"
auth_env = "LEAK_API_KEY"
chat_endpoint = "/chat/completions"

[aliases]
leak-alias = { id = "leak/model", provider = "leakco", tool_format = "text" }

[models."leak/model"]
name = "Leak Model"
provider = "leakco"
context_window = 8192
"#;

fn catalog_mentions_leak(artifact: &ProviderCatalogArtifact) -> bool {
    artifact.providers.iter().any(|p| p.id == "leakco")
        || artifact.aliases.iter().any(|a| a.name == "leak-alias")
        || artifact.models.iter().any(|m| m.id == "leak/model")
}

#[test]
fn embedded_artifact_generation_is_hermetic_against_ambient_overlay() {
    // Baseline generated with no ambient config at all.
    llm_config::clear_user_overrides();
    let baseline = artifact_embedded(None, None);
    let baseline_json = artifact_json_embedded(None, None).expect("baseline json");
    let baseline_ts = typescript_binding_embedded(None, None).expect("baseline ts");
    let baseline_swift = swift_binding_embedded(None, None).expect("baseline swift");

    assert!(
        !catalog_mentions_leak(&baseline),
        "baseline embedded catalog should not contain leak fixtures"
    );

    // Install an ambient overlay — the in-test proxy for a polluting
    // `~/.config/harn/providers.toml` / `HARN_PROVIDERS_CONFIG`. Hermetic
    // generation must ignore it entirely, so output stays byte-identical.
    {
        let _guard = install_overlay(POLLUTING_OVERLAY);

        // Sanity-check the contrast: the *runtime* catalog DOES reflect the
        // ambient overlay, so this is a genuine hermetic fork, not a no-op.
        assert!(
            catalog_mentions_leak(&artifact()),
            "runtime artifact() must still reflect the ambient overlay"
        );

        let polluted = artifact_embedded(None, None);
        assert!(
            !catalog_mentions_leak(&polluted),
            "embedded catalog leaked ambient overlay aliases/providers/models"
        );
        assert_eq!(
            artifact_json_embedded(None, None).expect("polluted json"),
            baseline_json,
            "embedded provider-catalog.json must be byte-identical with/without ambient overlay"
        );
        assert_eq!(
            typescript_binding_embedded(None, None).expect("polluted ts"),
            baseline_ts,
            "embedded TypeScript binding must be byte-identical with/without ambient overlay"
        );
        assert_eq!(
            swift_binding_embedded(None, None).expect("polluted swift"),
            baseline_swift,
            "embedded Swift binding must be byte-identical with/without ambient overlay"
        );
    }

    // After dropping the overlay, generation is unchanged.
    assert_eq!(
        artifact_json_embedded(None, None).expect("cleared json"),
        baseline_json
    );
}

#[test]
fn embedded_artifact_honors_explicit_declared_overlay() {
    llm_config::clear_user_overrides();
    let overlay = llm_config::parse_config_toml(POLLUTING_OVERLAY).expect("overlay parses");
    // An *explicit* declared overlay (e.g. a `--overlay` file named on the
    // command line) is reproducible input, not ambient machine state, so the
    // hermetic generator merges it on top of the embedded base.
    let artifact = artifact_embedded(Some(&overlay), None);
    assert!(
        catalog_mentions_leak(&artifact),
        "explicit overlay should be merged into the embedded catalog"
    );
}

#[test]
fn overlay_suppress_hides_route_aliases_and_variants() {
    let _guard = install_overlay(
        r#"
[suppress]
routes = ["together:Qwen/Qwen3-Coder-Next-FP8"]
"#,
    );
    let catalog = artifact();
    assert!(
        !catalog
            .models
            .iter()
            .any(|model| model.provider == "together" && model.id == "Qwen/Qwen3-Coder-Next-FP8"),
        "suppressed route must not export a model row"
    );
    assert!(
        !catalog
            .aliases
            .iter()
            .any(|alias| alias.provider == "together"
                && alias.model_id == "Qwen/Qwen3-Coder-Next-FP8"),
        "aliases of a suppressed route must not be exported"
    );
    assert!(
        !catalog
            .variants
            .iter()
            .any(|variant| variant.provider == "together"
                && variant.model_id == "Qwen/Qwen3-Coder-Next-FP8"),
        "recommendation variants must re-derive from surviving routes"
    );
}

#[test]
fn suppress_splits_on_first_colon_and_ignores_malformed_entries() {
    let _guard = install_overlay(
        r#"
[providers.private]
display_name = "Private"
base_url = "http://127.0.0.1:9000"
auth_style = "none"
chat_endpoint = "/v1/chat/completions"

[models."img:tag-a"]
name = "Img A"
provider = "private"
context_window = 8192

[models."img:tag-b"]
name = "Img B"
provider = "private"
context_window = 8192

[suppress]
routes = ["private:img:tag-a", "not-a-route"]
"#,
    );
    let catalog = artifact();
    assert!(
        !catalog.models.iter().any(|model| model.id == "img:tag-a"),
        "model ids containing colons are matched by splitting the selector on its first colon"
    );
    assert!(
        catalog.models.iter().any(|model| model.id == "img:tag-b"),
        "sibling routes are unaffected"
    );
}
