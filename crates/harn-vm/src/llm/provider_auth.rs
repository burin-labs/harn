use serde::{Deserialize, Serialize};

use crate::llm_config::{self, ProviderDef};
use crate::value::{VmError, VmValue};

/// Credential resolution state reported by Harn's dispatch authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCredentialStatus {
    Ok,
    Missing,
    NotRequired,
    Deferred,
}

impl ProviderCredentialStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Missing => "missing",
            Self::NotRequired => "not_required",
            Self::Deferred => "deferred",
        }
    }
}

/// Secret-free provider usability status for native hosts and VM projections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderAuthStatus {
    pub name: String,
    pub available: bool,
    pub credential_status: ProviderCredentialStatus,
}

/// The authority that selected a provider for a single-route dispatch.
///
/// This is deliberately separate from credential state: selection explains
/// why Harn is asking for a particular credential, while credential probing
/// decides whether that route can run. Keeping the two concepts separate
/// prevents a missing key from rewriting an explicit selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderSelectionSource {
    CallOption,
    Environment,
    DefaultEnvironment,
    ConfiguredDefault,
    ModelSelection,
    RoutingPolicy,
    Automatic,
}

#[derive(Debug)]
pub(crate) struct ResolvedProviderAuth {
    pub status: ProviderAuthStatus,
    credential: ResolvedProviderCredential,
}

impl ResolvedProviderAuth {
    pub(crate) fn into_api_key(self) -> Option<String> {
        match self.credential {
            ResolvedProviderCredential::Key(api_key) => Some(api_key),
            _ => None,
        }
    }
}

#[derive(Debug)]
enum ResolvedProviderCredential {
    Key(String),
    Missing,
    NotRequired,
    Deferred,
    ResolutionError(VmError),
}

/// Resolve one provider's usability through the exact credential path used by
/// dispatch. The returned status never contains secret material.
pub fn provider_auth_status(provider: &str) -> ProviderAuthStatus {
    let definition = llm_config::provider_config(provider);
    resolve_provider_auth_with_definition(provider, definition.as_ref()).status
}

/// Resolve all configured and runtime-registered providers in stable name order.
pub fn provider_auth_statuses() -> Vec<ProviderAuthStatus> {
    super::provider::register_default_providers();
    let mut names: std::collections::BTreeSet<String> =
        llm_config::provider_names().into_iter().collect();
    names.extend(super::provider::registered_provider_names());
    names
        .into_iter()
        .map(|name| provider_auth_status(&name))
        .collect()
}

pub fn available_provider_names() -> Vec<String> {
    llm_config::provider_names()
        .into_iter()
        .filter(|provider| provider_auth_status(provider).available)
        .collect()
}

pub(crate) fn provider_auth_status_with_definition(
    provider: &str,
    definition: &ProviderDef,
) -> ProviderAuthStatus {
    resolve_provider_auth_with_definition(provider, Some(definition)).status
}

pub(crate) fn resolve_provider_auth(provider: &str) -> ResolvedProviderAuth {
    let definition = llm_config::provider_config(provider);
    resolve_provider_auth_with_definition(provider, definition.as_ref())
}

fn resolve_provider_auth_with_definition(
    provider: &str,
    definition: Option<&ProviderDef>,
) -> ResolvedProviderAuth {
    let name = provider.to_string();
    let credential = if provider == "mock"
        || provider == "fake"
        || super::mock::cli_llm_mock_replay_active()
        || super::mock::builtin_llm_mock_active()
    {
        ResolvedProviderCredential::NotRequired
    } else if let Some(definition) = definition {
        if definition.is_credential_resolution_platform_managed() {
            ResolvedProviderCredential::Deferred
        } else if definition.auth_style == "none"
            || matches!(definition.auth_env, llm_config::AuthEnv::None)
        {
            ResolvedProviderCredential::NotRequired
        } else {
            resolved_credential_from_probe(probe_api_key(Some(definition)))
        }
    } else {
        resolved_credential_from_probe(probe_api_key(None))
    };
    let (available, credential_status) = match &credential {
        ResolvedProviderCredential::Key(_) => (true, ProviderCredentialStatus::Ok),
        ResolvedProviderCredential::Missing | ResolvedProviderCredential::ResolutionError(_) => {
            (false, ProviderCredentialStatus::Missing)
        }
        ResolvedProviderCredential::NotRequired => (true, ProviderCredentialStatus::NotRequired),
        ResolvedProviderCredential::Deferred => (true, ProviderCredentialStatus::Deferred),
    };
    ResolvedProviderAuth {
        status: ProviderAuthStatus {
            name,
            available,
            credential_status,
        },
        credential,
    }
}

fn resolved_credential_from_probe(
    result: Result<Option<String>, ProviderCredentialError>,
) -> ResolvedProviderCredential {
    match result {
        Ok(Some(api_key)) => ResolvedProviderCredential::Key(api_key),
        Ok(None) => ResolvedProviderCredential::NotRequired,
        Err(ProviderCredentialError::Missing) => ResolvedProviderCredential::Missing,
        Err(ProviderCredentialError::Resolution(error)) => {
            ResolvedProviderCredential::ResolutionError(error)
        }
    }
}

/// Resolve the provider credential used by dispatch.
pub fn resolve_api_key(provider: &str) -> Result<String, VmError> {
    resolve_api_key_for_selection(provider, inferred_provider_selection_source(provider))
}

pub(crate) fn resolve_api_key_for_selection(
    provider: &str,
    source: ProviderSelectionSource,
) -> Result<String, VmError> {
    let definition = llm_config::provider_config(provider);
    resolve_api_key_with_definition(provider, definition.as_ref(), source)
}

fn resolve_api_key_with_definition(
    provider: &str,
    definition: Option<&ProviderDef>,
    source: ProviderSelectionSource,
) -> Result<String, VmError> {
    let selection_hint = provider_selection_hint(provider, source);

    match resolve_provider_auth_with_definition(provider, definition).credential {
        ResolvedProviderCredential::Key(api_key) => Ok(api_key),
        ResolvedProviderCredential::NotRequired | ResolvedProviderCredential::Deferred => {
            Ok(String::new())
        }
        ResolvedProviderCredential::ResolutionError(error) => Err(error),
        ResolvedProviderCredential::Missing => {
            if let Some(definition) = definition {
                let aggregate_hint = no_credentials_message();
                let requirement = match &definition.auth_env {
                    llm_config::AuthEnv::Single(env) => {
                        format!("set {env} environment variable")
                    }
                    llm_config::AuthEnv::Multiple(envs) => {
                        format!("set one of {} environment variables", envs.join(", "))
                    }
                    llm_config::AuthEnv::None => return Ok(String::new()),
                };
                Err(missing_key_error(format!(
                    "Missing API key: {requirement}{selection_hint}\n{aggregate_hint}"
                )))
            } else {
                let aggregate_hint = no_credentials_message();
                Err(missing_key_error(format!(
                    "Missing API key: set ANTHROPIC_API_KEY environment variable{selection_hint}\n{aggregate_hint}"
                )))
            }
        }
    }
}

pub(crate) fn inferred_provider_selection_source(provider: &str) -> ProviderSelectionSource {
    if crate::stdlib::process::session_env_value("HARN_LLM_PROVIDER")
        .is_some_and(|selected| selected == provider)
    {
        ProviderSelectionSource::Environment
    } else if crate::stdlib::process::session_env_value("HARN_DEFAULT_PROVIDER")
        .is_some_and(|selected| selected.trim() == provider)
    {
        ProviderSelectionSource::DefaultEnvironment
    } else if llm_config::load_config()
        .default_provider
        .as_deref()
        .is_some_and(|selected| selected.trim() == provider)
    {
        ProviderSelectionSource::ConfiguredDefault
    } else {
        ProviderSelectionSource::Automatic
    }
}

fn provider_selection_hint(provider: &str, source: ProviderSelectionSource) -> String {
    let selected_via = match source {
        ProviderSelectionSource::CallOption => "the llm_call `provider` option".to_string(),
        ProviderSelectionSource::Environment => "HARN_LLM_PROVIDER".to_string(),
        ProviderSelectionSource::DefaultEnvironment => "HARN_DEFAULT_PROVIDER".to_string(),
        ProviderSelectionSource::ConfiguredDefault => {
            let paths = llm_config::loaded_config_paths();
            if paths.is_empty() {
                "the built-in configured default".to_string()
            } else {
                format!(
                    "the configured default (provider config loaded from {})",
                    paths
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
        ProviderSelectionSource::ModelSelection => "the selected model".to_string(),
        ProviderSelectionSource::RoutingPolicy => "the routing policy".to_string(),
        ProviderSelectionSource::Automatic => "the automatic provider preference order".to_string(),
    };
    format!(
        " (provider '{provider}' selected via {selected_via}; set HARN_LLM_PROVIDER=mock to run \
         without a provider)"
    )
}

#[derive(Debug)]
enum ProviderCredentialError {
    Missing,
    Resolution(VmError),
}

/// Read one credential-bearing variable through the session environment rather
/// than the raw process environment.
///
/// Under an isolated policy no credential resolves — that is what makes an eval
/// run reproducible instead of quietly picking up the launcher's key. Under a
/// granted policy, a declared variable resolves to the granted value, so harn's
/// own `llm_call` can use it.
///
/// A grant-resolution failure (an unresolvable `secret_store` pointer) is a
/// missing credential from this path's point of view; `resolve_api_key` renders
/// the same loud "Missing API key" guidance it renders for an unset variable,
/// and the spawn boundary still reports the underlying `MissingSecret`.
fn session_auth_env(name: &str) -> Option<String> {
    crate::stdlib::process::session_env_var(name)
        .ok()
        .flatten()
        .filter(|value| !value.is_empty())
}

fn probe_api_key(
    definition: Option<&ProviderDef>,
) -> Result<Option<String>, ProviderCredentialError> {
    let Some(definition) = definition else {
        return session_auth_env("ANTHROPIC_API_KEY")
            .map(Some)
            .ok_or(ProviderCredentialError::Missing);
    };
    match &definition.auth_env {
        llm_config::AuthEnv::None => Ok(None),
        llm_config::AuthEnv::Single(env) => {
            let raw = session_auth_env(env).ok_or(ProviderCredentialError::Missing)?;
            resolve_auth_env_value(env, &raw)
                .map_err(ProviderCredentialError::Resolution)
                .map(Some)
        }
        llm_config::AuthEnv::Multiple(envs) => {
            for env in envs {
                let Some(raw) = session_auth_env(env) else {
                    continue;
                };
                return resolve_auth_env_value(env, &raw)
                    .map_err(ProviderCredentialError::Resolution)
                    .map(Some);
            }
            Err(ProviderCredentialError::Missing)
        }
    }
}

fn missing_key_error(message: String) -> VmError {
    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(message)))
}

fn resolve_auth_env_value(env_name: &str, raw: &str) -> Result<String, VmError> {
    match crate::secrets::resolve_secret_ref_to_string(raw) {
        Ok(Some(secret)) => Ok(secret),
        Ok(None) => Ok(raw.to_string()),
        Err(error) => Err(missing_key_error(format!(
            "Failed to resolve API key secret reference from {env_name}: {error}"
        ))),
    }
}

/// Canonical documentation entry point for provider credential setup.
pub const PROVIDER_SETUP_DOCS_URL: &str = "https://harnlang.com/provider-setup.html";

/// Whether this provider needs a credential at all. Local servers such as
/// Ollama declare `auth_style = "none"` and are usable with no key.
fn requires_credential(provider: &str) -> bool {
    llm_config::provider_config(provider).is_some_and(|definition| definition.auth_style != "none")
}

/// Credential env vars accepted by the named providers, in their order, with
/// duplicates removed. Providers that need no key contribute nothing.
///
/// `primary_only` keeps just the first variable each provider accepts. A
/// provider that takes alternatives (Gemini reads `GEMINI_API_KEY` or
/// `GOOGLE_API_KEY`) would otherwise spend two slots in a short list to say
/// one thing; `harn doctor` and the docs still name every alternative.
fn credential_env_names(providers: &[String], primary_only: bool) -> Vec<String> {
    let mut envs: Vec<String> = Vec::new();
    for name in providers {
        let Some(definition) = llm_config::provider_config(name) else {
            continue;
        };
        if definition.auth_style == "none" {
            continue;
        }
        let accepted = llm_config::auth_env_names(&definition.auth_env);
        let accepted = if primary_only {
            accepted.into_iter().take(1).collect()
        } else {
            accepted
        };
        for env in accepted {
            if !envs.contains(&env) {
                envs.push(env);
            }
        }
    }
    envs
}

/// Build the canonical no-credentials guidance from the live catalog.
///
/// The catalog carries dozens of providers, and printing every accepted
/// variable buries the one thing a reader needs: a name they recognise and a
/// next step. So this names the catalog's curated short list, says how many
/// other providers exist, and points at the two places that hold the complete
/// answer — `harn doctor` locally and the setup guide online. The short list
/// is catalog data, not a literal here, so the curated set keeps one owner.
pub fn no_credentials_message() -> String {
    let all_providers = llm_config::provider_names();
    let featured = llm_config::featured_provider_names();

    // An overlay can suppress every featured provider. Falling back to the
    // full catalog keeps the message useful instead of empty.
    let shown_envs = match credential_env_names(&featured, true) {
        envs if envs.is_empty() => credential_env_names(&all_providers, false),
        envs => envs,
    };
    if shown_envs.is_empty() {
        return format!(
            "No LLM providers are declared in the loaded catalog. Add one, then read \
             {PROVIDER_SETUP_DOCS_URL}."
        );
    }

    let featured_keyed = featured
        .iter()
        .filter(|name| requires_credential(name))
        .count();
    let catalog_keyed = all_providers
        .iter()
        .filter(|name| requires_credential(name))
        .count();
    let remaining = catalog_keyed.saturating_sub(featured_keyed);
    let more_hint = if remaining == 0 {
        String::new()
    } else {
        format!(" Harn supports {remaining} more providers.")
    };

    let keyless: Vec<String> = featured
        .iter()
        .filter(|name| !requires_credential(name))
        .map(|name| format!("`{name}`"))
        .collect();
    let keyless_hint = if keyless.is_empty() {
        String::new()
    } else {
        format!(" {} runs locally and needs no key.", keyless.join(" and "))
    };

    format!(
        "No LLM provider credentials found. Set one of these environment variables to an API key \
         or a harn-secret://namespace/name reference: {envs}.{keyless_hint}{more_hint} \
         Run `harn doctor` to print every provider and the variable it reads, or read \
         {PROVIDER_SETUP_DOCS_URL}. `harn models recommend` suggests a setup for this machine.",
        envs = shown_envs.join(", "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ScopedEnv {
        name: &'static str,
        previous: Option<String>,
    }

    impl ScopedEnv {
        fn set(name: &'static str, value: &str) -> Self {
            let previous = std::env::var(name).ok();
            unsafe { std::env::set_var(name, value) };
            Self { name, previous }
        }

        fn unset(name: &'static str) -> Self {
            let previous = std::env::var(name).ok();
            unsafe { std::env::remove_var(name) };
            Self { name, previous }
        }
    }

    impl Drop for ScopedEnv {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var(self.name, value),
                    None => std::env::remove_var(self.name),
                }
            }
        }
    }

    #[test]
    fn typed_status_covers_each_credential_resolution_class() {
        let _guard = crate::llm::env_guard();
        let _anthropic = ScopedEnv::unset("ANTHROPIC_API_KEY");
        let _azure_key = ScopedEnv::unset("AZURE_OPENAI_API_KEY");
        let _azure_token = ScopedEnv::unset("AZURE_OPENAI_AD_TOKEN");
        let _azure_bearer = ScopedEnv::unset("AZURE_OPENAI_BEARER_TOKEN");

        assert_eq!(
            provider_auth_status("anthropic").credential_status,
            ProviderCredentialStatus::Missing
        );
        assert_eq!(
            provider_auth_status("ollama").credential_status,
            ProviderCredentialStatus::NotRequired
        );
        assert_eq!(
            provider_auth_status("bedrock").credential_status,
            ProviderCredentialStatus::Deferred
        );
        assert_eq!(
            provider_auth_status("vertex").credential_status,
            ProviderCredentialStatus::Deferred
        );
        assert_eq!(resolve_api_key("bedrock").unwrap(), "");
        assert_eq!(resolve_api_key("vertex").unwrap(), "");
        assert!(resolve_api_key("azure_openai").is_err());
    }

    #[test]
    fn missing_credential_diagnostic_names_the_actual_selection_authority() {
        let _guard = crate::llm::env_guard();
        let env = crate::test_env::test_env_guard();
        env.set("HARN_LLM_PROVIDER", "anthropic");
        let _missing_key = ScopedEnv::unset("ANTHROPIC_API_KEY");

        let message = match resolve_api_key("anthropic").unwrap_err() {
            VmError::Thrown(VmValue::String(message)) => message.to_string(),
            other => format!("{other:?}"),
        };
        assert!(
            message.contains("provider 'anthropic' selected via HARN_LLM_PROVIDER"),
            "got: {message}"
        );
        assert!(!message.contains(" or the provider catalog"));
        assert!(!message.contains("<built-in defaults>"));
    }

    #[test]
    fn selection_hints_name_only_the_authority_that_fired() {
        let explicit = provider_selection_hint("openai", ProviderSelectionSource::CallOption);
        assert!(explicit.contains("selected via the llm_call `provider` option"));
        assert!(!explicit.contains("config loaded from"));

        let automatic = provider_selection_hint("openai", ProviderSelectionSource::Automatic);
        assert!(automatic.contains("selected via the automatic provider preference order"));
        assert!(!automatic.contains("config loaded from"));

        let configured =
            provider_selection_hint("anthropic", ProviderSelectionSource::ConfiguredDefault);
        if llm_config::loaded_config_paths().is_empty() {
            assert!(configured.contains("selected via the built-in configured default"));
            assert!(!configured.contains("config loaded from"));
        } else {
            assert!(configured.contains("configured default (provider config loaded from"));
        }
    }

    #[test]
    fn credential_authority_matrix_is_fail_closed_and_secret_free() {
        let _guard = crate::llm::env_guard();
        const SECRET: &str = "sk-authority-matrix-sentinel";

        let allowed_key = ScopedEnv::set("ANTHROPIC_API_KEY", SECRET);
        assert_eq!(resolve_api_key("anthropic").unwrap(), SECRET, "allowed");
        assert_eq!(
            provider_auth_status("anthropic").credential_status,
            ProviderCredentialStatus::Ok
        );

        {
            let _environment =
                ScopedEnvironment::install(crate::security::SessionEnvironment::isolated());
            let error = resolve_api_key("anthropic").unwrap_err().to_string();
            assert!(error.contains("Missing API key"), "denied: {error}");
            assert!(!error.contains(SECRET), "denied diagnostic leaked the key");
        }

        drop(allowed_key);
        let missing_key = ScopedEnv::unset("ANTHROPIC_API_KEY");
        let missing = resolve_api_key("anthropic").unwrap_err().to_string();
        assert!(missing.contains("Missing API key"), "missing: {missing}");
        assert!(!missing.contains(SECRET));

        drop(missing_key);
        let _malformed_key = ScopedEnv::set("ANTHROPIC_API_KEY", "harn-secret://malformed");
        let malformed = resolve_api_key("anthropic").unwrap_err().to_string();
        assert!(
            malformed.contains("Failed to resolve API key secret reference"),
            "malformed: {malformed}"
        );
        assert!(!malformed.contains(SECRET));
        assert_eq!(
            provider_auth_status("anthropic").credential_status,
            ProviderCredentialStatus::Missing
        );
    }

    #[test]
    fn status_and_dispatch_resolve_the_same_secret_reference_without_caching() {
        let _guard = crate::llm::env_guard();
        let _providers = ScopedEnv::set("HARN_SECRET_PROVIDERS", "env");
        let _reference = ScopedEnv::set(
            "ANTHROPIC_API_KEY",
            "harn-secret://provider/anthropic-api-key",
        );
        let secret = ScopedEnv::set("HARN_SECRET_PROVIDER_ANTHROPIC_API_KEY", "sk-from-ref");

        assert_eq!(resolve_api_key("anthropic").unwrap(), "sk-from-ref");
        assert_eq!(
            provider_auth_status("anthropic").credential_status,
            ProviderCredentialStatus::Ok
        );

        drop(secret);
        let _missing = ScopedEnv::unset("HARN_SECRET_PROVIDER_ANTHROPIC_API_KEY");
        assert_eq!(
            provider_auth_status("anthropic").credential_status,
            ProviderCredentialStatus::Missing
        );
        let error = resolve_api_key("anthropic").unwrap_err();
        let message = match error {
            VmError::Thrown(VmValue::String(message)) => message.to_string(),
            other => format!("{other:?}"),
        };
        assert!(
            message.contains("Failed to resolve API key secret reference from ANTHROPIC_API_KEY")
        );
        assert!(message.contains("provider/anthropic-api-key"));
        assert!(!message.contains("sk-from-ref"));
    }

    #[test]
    fn harness_session_provider_resolves_reference_without_global_secret_state() {
        let _guard = crate::llm::env_guard();
        let _reference = ScopedEnv::set(
            "ANTHROPIC_API_KEY",
            "harn-secret://burin.provider-auth/anthropic-api-key",
        );
        let _providers = ScopedEnv::set("HARN_SECRET_PROVIDERS", "env");
        let _global_secret = ScopedEnv::unset("HARN_SECRET_BURIN_PROVIDER_AUTH_ANTHROPIC_API_KEY");
        let provider = crate::secrets::MemorySecretProvider::new("burin-session").with_secret(
            crate::secrets::SecretId::new("burin.provider-auth", "anthropic-api-key"),
            "sk-process-local",
        );

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
            .block_on(crate::secrets::with_active_secret_provider(
                Some(std::sync::Arc::new(provider)),
                async {
                    assert_eq!(resolve_api_key("anthropic").unwrap(), "sk-process-local");
                    assert_eq!(
                        provider_auth_status("anthropic").credential_status,
                        ProviderCredentialStatus::Ok
                    );
                },
            ));

        assert_eq!(
            provider_auth_status("anthropic").credential_status,
            ProviderCredentialStatus::Missing,
            "the process-local provider must not escape its task"
        );
    }

    /// Install a environment policy for the duration of a test and clear it on
    /// drop, so a panicking assertion cannot leak a profile into a sibling test
    /// sharing the thread.
    struct ScopedEnvironment;

    impl ScopedEnvironment {
        fn install(environment: crate::security::SessionEnvironment) -> Self {
            crate::stdlib::process::set_session_environment(Some(environment));
            Self
        }
    }

    impl Drop for ScopedEnvironment {
        fn drop(&mut self) {
            crate::stdlib::process::set_session_environment(None);
        }
    }

    #[test]
    fn isolated_policy_hides_the_launcher_key_from_dispatch() {
        // The launcher has a key; the session is isolated. Harn's own credential
        // path must not see it, or a "no credentials" eval silently runs against
        // whatever the operator happened to have exported.
        let _guard = crate::llm::env_guard();
        let _key = ScopedEnv::set("ANTHROPIC_API_KEY", "sk-launcher");

        assert_eq!(
            provider_auth_status("anthropic").credential_status,
            ProviderCredentialStatus::Ok,
            "outside a session, bootstrap reads still use the process env"
        );

        let _environment =
            ScopedEnvironment::install(crate::security::SessionEnvironment::isolated());
        assert_eq!(
            provider_auth_status("anthropic").credential_status,
            ProviderCredentialStatus::Missing,
            "isolated must close the in-process credential path, not just subprocess env"
        );
        let message = match resolve_api_key("anthropic").unwrap_err() {
            VmError::Thrown(VmValue::String(message)) => message.to_string(),
            other => format!("{other:?}"),
        };
        assert!(message.contains("Missing API key"));
        assert!(!message.contains("sk-launcher"), "error leaked the key");
    }

    #[test]
    fn lane_grant_reaches_harns_own_dispatch() {
        // A granted policy must make its provider key usable
        // for its own llm_call, not only hand it to subprocesses. The launcher
        // variable here is deliberately NOT the provider's auth env var, so the
        // key can only arrive through the grant's `expose_as_env`.
        use crate::security::{
            EnvironmentPolicyKind, GrantSourceSpec, GrantSpec, SessionEnvironment,
        };

        let _guard = crate::llm::env_guard();
        let _absent = ScopedEnv::unset("ANTHROPIC_API_KEY");
        let _source = ScopedEnv::set("LAUNCHER_ANTHROPIC_SECRET", "sk-granted");

        let granted = SessionEnvironment::launch(
            EnvironmentPolicyKind::Granted,
            vec![GrantSpec {
                name: "anthropic".to_string(),
                source: GrantSourceSpec::Env {
                    var: "LAUNCHER_ANTHROPIC_SECRET".to_string(),
                },
                expose_as_env: Some("ANTHROPIC_API_KEY".to_string()),
                for_command: None,
            }],
            &|name| std::env::var(name).ok(),
        )
        .expect("granted policy launch");
        let _environment = ScopedEnvironment::install(granted);

        assert_eq!(
            provider_auth_status("anthropic").credential_status,
            ProviderCredentialStatus::Ok
        );
        assert_eq!(resolve_api_key("anthropic").unwrap(), "sk-granted");
    }

    #[test]
    fn an_empty_auth_variable_is_a_missing_credential() {
        // An exported-but-empty key is not a credential. The multi-var branch
        // always skipped empties; the single-var branch now agrees, so a blank
        // export fails loudly at resolution instead of reaching a provider as an
        // empty bearer token.
        let _guard = crate::llm::env_guard();
        let _blank = ScopedEnv::set("ANTHROPIC_API_KEY", "");
        assert_eq!(
            provider_auth_status("anthropic").credential_status,
            ProviderCredentialStatus::Missing
        );
        assert!(resolve_api_key("anthropic").is_err());
    }

    #[test]
    fn status_serialization_is_stable_and_secret_free() {
        let status = ProviderAuthStatus {
            name: "example".to_string(),
            available: true,
            credential_status: ProviderCredentialStatus::Deferred,
        };
        assert_eq!(
            serde_json::to_value(status).unwrap(),
            serde_json::json!({
                "name": "example",
                "available": true,
                "credential_status": "deferred",
            })
        );
    }

    #[test]
    fn available_provider_names_uses_dispatch_semantics() {
        let _guard = crate::llm::env_guard();
        let _vertex_token = ScopedEnv::unset("VERTEX_AI_ACCESS_TOKEN");
        let _google_token = ScopedEnv::unset("GOOGLE_OAUTH_ACCESS_TOKEN");
        let _google_credentials = ScopedEnv::unset("GOOGLE_APPLICATION_CREDENTIALS");

        let available = available_provider_names();
        assert!(available.iter().any(|name| name == "bedrock"));
        assert!(available.iter().any(|name| name == "vertex"));
    }
}
