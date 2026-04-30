//! Azure OpenAI provider.
//!
//! Azure uses the OpenAI chat-completions request body, but routes by
//! deployment name in the URL and authenticates with either `api-key` or
//! Microsoft Entra bearer tokens.

use crate::llm::api::{DeltaSender, LlmRequestPayload, LlmResult};
use crate::llm::provider::{LlmProvider, LlmProviderChat};
use crate::llm::providers::common::{
    apply_provider_overrides, maybe_emit_delta, percent_encode_path_segment, vm_err,
};
use crate::value::VmError;

use super::openai_compat::OpenAiCompatibleProvider;

pub(crate) const DEFAULT_API_VERSION: &str = "2024-10-21";

pub(crate) struct AzureOpenAiProvider;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AzureAuth {
    ApiKey(String),
    Bearer(String),
}

impl AzureOpenAiProvider {
    pub(crate) fn build_request_body(request: &LlmRequestPayload) -> serde_json::Value {
        let mut body = OpenAiCompatibleProvider::build_request_body(request, false);
        if let Some(obj) = body.as_object_mut() {
            // Azure deployment routing supplies the model identity in the path.
            obj.remove("model");
        }
        body
    }

    pub(crate) fn endpoint_url(request: &LlmRequestPayload) -> Result<String, VmError> {
        let pdef = crate::llm_config::provider_config("azure_openai");
        let base_url = pdef
            .as_ref()
            .map(crate::llm_config::resolve_base_url)
            .unwrap_or_default();
        let base_url = base_url.trim_end_matches('/');
        if base_url.is_empty() || base_url.contains('{') {
            return Err(vm_err(
                "Azure OpenAI endpoint is not configured; set AZURE_OPENAI_ENDPOINT",
            ));
        }
        let deployment = std::env::var("AZURE_OPENAI_DEPLOYMENT")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| request.model.clone());
        if deployment.trim().is_empty() {
            return Err(vm_err(
                "Azure OpenAI deployment is not configured; set model or AZURE_OPENAI_DEPLOYMENT",
            ));
        }
        let api_version = std::env::var("AZURE_OPENAI_API_VERSION")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_API_VERSION.to_string());
        Ok(format!(
            "{base_url}/openai/deployments/{}/chat/completions?api-version={api_version}",
            percent_encode_path_segment(deployment.trim())
        ))
    }

    pub(crate) fn resolve_auth(api_key: &str) -> Result<AzureAuth, VmError> {
        if let Ok(key) = std::env::var("AZURE_OPENAI_API_KEY") {
            if !key.trim().is_empty() {
                return Ok(AzureAuth::ApiKey(key));
            }
        }
        for env_name in ["AZURE_OPENAI_AD_TOKEN", "AZURE_OPENAI_BEARER_TOKEN"] {
            if let Ok(token) = std::env::var(env_name) {
                if !token.trim().is_empty() {
                    return Ok(AzureAuth::Bearer(token));
                }
            }
        }
        if !api_key.trim().is_empty() {
            return Ok(AzureAuth::ApiKey(api_key.to_string()));
        }
        Err(vm_err(
            "Missing Azure OpenAI credentials: set AZURE_OPENAI_API_KEY or AZURE_OPENAI_AD_TOKEN",
        ))
    }

    pub(crate) async fn chat_impl(
        &self,
        request: &LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> Result<LlmResult, VmError> {
        let url = Self::endpoint_url(request)?;
        let auth = Self::resolve_auth(&request.api_key)?;
        let mut body = Self::build_request_body(request);
        apply_provider_overrides(&mut body, request.provider_overrides.as_ref());
        let mut req = crate::llm::shared_blocking_client()
            .post(url)
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(request.resolve_timeout()))
            .json(&body);
        req = match auth {
            AzureAuth::ApiKey(key) => req.header("api-key", key),
            AzureAuth::Bearer(token) => req.header("Authorization", format!("Bearer {token}")),
        };
        let response = req
            .send()
            .await
            .map_err(|error| vm_err(format!("azure_openai API error: {error}")))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(vm_err(format!("azure_openai HTTP {status}: {body}")));
        }
        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|error| vm_err(format!("azure_openai response parse error: {error}")))?;
        let resolved = crate::llm::helpers::ResolvedProvider::resolve("openai");
        let result = crate::llm::api::parse_llm_response_for_provider(
            &json,
            "azure_openai",
            &request.model,
            &resolved,
        )?;
        maybe_emit_delta(delta_tx, &result.text);
        Ok(result)
    }
}

impl LlmProvider for AzureOpenAiProvider {
    fn name(&self) -> &str {
        "azure_openai"
    }
}

impl LlmProviderChat for AzureOpenAiProvider {
    fn chat<'a>(
        &'a self,
        request: &'a LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<LlmResult, VmError>> + 'a>> {
        Box::pin(self.chat_impl(request, delta_tx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::api::{LlmRequestPayload, ThinkingConfig};
    use crate::llm::env_lock;
    use serde_json::json;

    struct ScopedEnv {
        key: &'static str,
        prev: Option<String>,
    }

    impl ScopedEnv {
        fn set(key: &'static str, value: &str) -> Self {
            let prev = std::env::var(key).ok();
            unsafe { std::env::set_var(key, value) };
            Self { key, prev }
        }
        fn remove(key: &'static str) -> Self {
            let prev = std::env::var(key).ok();
            unsafe { std::env::remove_var(key) };
            Self { key, prev }
        }
    }

    impl Drop for ScopedEnv {
        fn drop(&mut self) {
            match &self.prev {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    #[test]
    fn deployment_url_uses_model_and_api_version() {
        let _guard = env_lock().lock().unwrap();
        let _endpoint = ScopedEnv::set("AZURE_OPENAI_ENDPOINT", "https://acct.openai.azure.com/");
        let _api_version = ScopedEnv::set("AZURE_OPENAI_API_VERSION", "2025-01-01-preview");
        let _deployment = ScopedEnv::remove("AZURE_OPENAI_DEPLOYMENT");
        let request = base_request();
        let url = AzureOpenAiProvider::endpoint_url(&request).expect("url");
        assert_eq!(
            url,
            "https://acct.openai.azure.com/openai/deployments/gpt-4o-prod/chat/completions?api-version=2025-01-01-preview"
        );
    }

    #[test]
    fn auth_prefers_api_key_then_bearer_token() {
        let _guard = env_lock().lock().unwrap();
        let _key = ScopedEnv::set("AZURE_OPENAI_API_KEY", "key");
        let _token = ScopedEnv::set("AZURE_OPENAI_AD_TOKEN", "token");
        assert_eq!(
            AzureOpenAiProvider::resolve_auth("").expect("auth"),
            AzureAuth::ApiKey("key".to_string())
        );
        drop(_key);
        assert_eq!(
            AzureOpenAiProvider::resolve_auth("").expect("auth"),
            AzureAuth::Bearer("token".to_string())
        );
    }

    #[test]
    fn request_body_removes_model_because_deployment_routes() {
        let body = AzureOpenAiProvider::build_request_body(&base_request());
        assert!(body.get("model").is_none());
        assert_eq!(
            body["messages"][0],
            json!({"role": "user", "content": "hello"})
        );
    }

    fn base_request() -> LlmRequestPayload {
        LlmRequestPayload {
            provider: "azure_openai".to_string(),
            model: "gpt-4o-prod".to_string(),
            api_key: String::new(),
            fallback_chain: Vec::new(),
            messages: vec![json!({"role": "user", "content": "hello"})],
            system: None,
            max_tokens: 32,
            temperature: Some(0.0),
            top_p: None,
            top_k: None,
            stop: None,
            seed: None,
            frequency_penalty: None,
            presence_penalty: None,
            output_format: crate::llm::api::OutputFormat::Text,
            response_format: None,
            json_schema: None,
            thinking: ThinkingConfig::Disabled,
            anthropic_beta_features: Vec::new(),
            vision: false,
            native_tools: None,
            tool_choice: None,
            cache: false,
            timeout: None,
            stream: false,
            provider_overrides: None,
            prefill: None,
            session_id: None,
        }
    }
}
