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
    let definition = llm_config::provider_config(provider);
    resolve_api_key_with_definition(provider, definition.as_ref())
}

fn resolve_api_key_with_definition(
    provider: &str,
    definition: Option<&ProviderDef>,
) -> Result<String, VmError> {
    let selection_hint = {
        let config_path = llm_config::loaded_config_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<built-in defaults>".to_string());
        format!(
            " (provider '{provider}' selected via LLM_PROVIDER / llm.toml @ {config_path}; \
             set HARN_LLM_PROVIDER=mock or LLM_PROVIDER=mock for offline use)"
        )
    };

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

/// Build the canonical no-credentials guidance from the live catalog.
pub fn no_credentials_message() -> String {
    let mut envs = Vec::new();
    for name in llm_config::provider_names() {
        if let Some(definition) = llm_config::provider_config(&name) {
            if definition.auth_style == "none" {
                continue;
            }
            for env in llm_config::auth_env_names(&definition.auth_env) {
                if !envs.contains(&env) {
                    envs.push(env);
                }
            }
        }
    }
    envs.sort();
    envs.dedup();
    let env_list = if envs.is_empty() {
        "(no providers declared)".to_string()
    } else {
        envs.join(", ")
    };
    format!(
        "No LLM providers configured. Set one of these env vars to an API key or \
         harn-secret://namespace/name reference: {env_list} (or run a local Ollama). \
         For diagnostics: `harn doctor`. For a recommended setup: `harn models recommend` \
         (when available)."
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
