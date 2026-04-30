//! Google Vertex AI Gemini provider.
//!
//! Vertex exposes Gemini through `projects/{project}/locations/{location}/...`
//! routes and Google OAuth bearer tokens. The request mapper keeps Harn's
//! canonical OpenAI-like message list at the boundary and emits Vertex
//! `generateContent` JSON.

use crate::llm::api::{DeltaSender, LlmRequestPayload, LlmResult};
use crate::llm::provider::{LlmProvider, LlmProviderChat};
use crate::llm::providers::common::{
    apply_provider_overrides, maybe_emit_delta, percent_encode_path_segment, request_text_content,
    vm_err,
};
use crate::value::VmError;

pub(crate) struct VertexProvider;

#[derive(Debug, serde::Deserialize)]
struct ServiceAccountKey {
    client_email: String,
    private_key: String,
    #[serde(default)]
    token_uri: Option<String>,
    #[serde(default)]
    project_id: Option<String>,
}

#[derive(Debug, serde::Serialize)]
struct ServiceAccountClaims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    exp: usize,
    iat: usize,
}

impl VertexProvider {
    pub(crate) fn build_request_body(request: &LlmRequestPayload) -> serde_json::Value {
        let mut contents = Vec::new();
        let mut system_parts = Vec::new();
        if let Some(system) = request.system.as_deref() {
            if !system.is_empty() {
                system_parts.push(serde_json::json!({"text": system}));
            }
        }
        for message in &request.messages {
            let role = match message.get("role").and_then(|value| value.as_str()) {
                Some("assistant") => "model",
                Some("system") => {
                    let text = request_text_content(message);
                    if !text.is_empty() {
                        system_parts.push(serde_json::json!({"text": text}));
                    }
                    continue;
                }
                _ => "user",
            };
            let text = request_text_content(message);
            if text.is_empty() {
                continue;
            }
            contents.push(serde_json::json!({
                "role": role,
                "parts": [{"text": text}],
            }));
        }
        if let Some(prefill) = request.prefill.as_deref() {
            if !prefill.is_empty() {
                contents.push(serde_json::json!({
                    "role": "model",
                    "parts": [{"text": prefill}],
                }));
            }
        }

        let mut body = serde_json::json!({ "contents": contents });
        if !system_parts.is_empty() {
            body["systemInstruction"] = serde_json::json!({ "parts": system_parts });
        }
        let mut generation = serde_json::Map::new();
        if request.max_tokens > 0 {
            generation.insert(
                "maxOutputTokens".to_string(),
                serde_json::json!(request.max_tokens),
            );
        }
        if let Some(temp) = request.temperature {
            generation.insert("temperature".to_string(), serde_json::json!(temp));
        }
        if let Some(top_p) = request.top_p {
            generation.insert("topP".to_string(), serde_json::json!(top_p));
        }
        if let Some(stop) = request.stop.as_ref() {
            generation.insert("stopSequences".to_string(), serde_json::json!(stop));
        }
        if request.response_format.as_deref() == Some("json") {
            generation.insert(
                "responseMimeType".to_string(),
                serde_json::json!("application/json"),
            );
            if let Some(schema) = request.json_schema.as_ref() {
                generation.insert("responseSchema".to_string(), schema.clone());
            }
        }
        if !generation.is_empty() {
            body["generationConfig"] = serde_json::Value::Object(generation);
        }
        if let Some(tools) = vertex_tools(request.native_tools.as_deref()) {
            body["tools"] = tools;
        }
        body
    }

    pub(crate) fn endpoint_url(request: &LlmRequestPayload) -> Result<String, VmError> {
        let project = std::env::var("VERTEX_AI_PROJECT")
            .or_else(|_| std::env::var("GOOGLE_CLOUD_PROJECT"))
            .or_else(|_| service_account_project())
            .map_err(|_| vm_err("Vertex AI project is not configured; set VERTEX_AI_PROJECT"))?;
        let location = std::env::var("VERTEX_AI_LOCATION")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "us-central1".to_string());
        let pdef = crate::llm_config::provider_config("vertex");
        let base_url = pdef
            .as_ref()
            .map(crate::llm_config::resolve_base_url)
            .unwrap_or_else(|| "https://aiplatform.googleapis.com/v1".to_string());
        let base_url = base_url.trim_end_matches('/');
        let model = if request.model.starts_with("projects/") {
            request.model.clone()
        } else {
            format!(
                "projects/{}/locations/{}/publishers/google/models/{}",
                percent_encode_path_segment(&project),
                percent_encode_path_segment(&location),
                percent_encode_path_segment(&request.model)
            )
        };
        Ok(format!("{base_url}/{model}:generateContent"))
    }

    async fn bearer_token(api_key: &str) -> Result<String, VmError> {
        for env_name in ["VERTEX_AI_ACCESS_TOKEN", "GOOGLE_OAUTH_ACCESS_TOKEN"] {
            if let Ok(token) = std::env::var(env_name) {
                if !token.trim().is_empty() {
                    return Ok(token);
                }
            }
        }
        if !api_key.trim().is_empty() && !api_key.trim().starts_with('/') {
            return Ok(api_key.to_string());
        }
        let key_path = std::env::var("GOOGLE_APPLICATION_CREDENTIALS")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| (!api_key.trim().is_empty()).then(|| api_key.to_string()))
            .ok_or_else(|| {
                vm_err(
                    "Missing Vertex AI credentials: set VERTEX_AI_ACCESS_TOKEN or GOOGLE_APPLICATION_CREDENTIALS",
                )
            })?;
        exchange_service_account_token(&key_path).await
    }

    pub(crate) async fn chat_impl(
        &self,
        request: &LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> Result<LlmResult, VmError> {
        let url = Self::endpoint_url(request)?;
        let token = Self::bearer_token(&request.api_key).await?;
        let mut body = Self::build_request_body(request);
        apply_provider_overrides(&mut body, request.provider_overrides.as_ref());
        let response = crate::llm::shared_blocking_client()
            .post(url)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {token}"))
            .timeout(std::time::Duration::from_secs(request.resolve_timeout()))
            .json(&body)
            .send()
            .await
            .map_err(|error| vm_err(format!("vertex API error: {error}")))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(vm_err(format!("vertex HTTP {status}: {body}")));
        }
        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|error| vm_err(format!("vertex response parse error: {error}")))?;
        let result = parse_vertex_response(&json, &request.model)?;
        maybe_emit_delta(delta_tx, &result.text);
        Ok(result)
    }
}

impl LlmProvider for VertexProvider {
    fn name(&self) -> &str {
        "vertex"
    }
}

impl LlmProviderChat for VertexProvider {
    fn chat<'a>(
        &'a self,
        request: &'a LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<LlmResult, VmError>> + 'a>> {
        Box::pin(self.chat_impl(request, delta_tx))
    }
}

fn vertex_tools(tools: Option<&[serde_json::Value]>) -> Option<serde_json::Value> {
    let mut declarations = Vec::new();
    for tool in tools.unwrap_or_default() {
        let function = tool.get("function").unwrap_or(tool);
        let Some(name) = function.get("name").and_then(|value| value.as_str()) else {
            continue;
        };
        let mut declaration = serde_json::json!({ "name": name });
        if let Some(description) = function.get("description") {
            declaration["description"] = description.clone();
        }
        if let Some(parameters) = function
            .get("parameters")
            .or_else(|| function.get("input_schema"))
        {
            declaration["parameters"] = parameters.clone();
        }
        declarations.push(declaration);
    }
    (!declarations.is_empty())
        .then(|| serde_json::json!([{ "functionDeclarations": declarations }]))
}

fn parse_vertex_response(json: &serde_json::Value, model: &str) -> Result<LlmResult, VmError> {
    if let Some(error) = json["error"]["message"].as_str() {
        return Err(vm_err(format!("vertex API error: {error}")));
    }
    let mut result = crate::llm::providers::common::empty_result("vertex", model);
    if let Some(parts) = json["candidates"][0]["content"]["parts"].as_array() {
        for part in parts {
            if let Some(text) = part.get("text").and_then(|value| value.as_str()) {
                result.text.push_str(text);
                result.blocks.push(serde_json::json!({
                    "type": "output_text",
                    "text": text,
                    "visibility": "public",
                }));
            }
            if let Some(call) = part.get("functionCall") {
                let name = call
                    .get("name")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                let args = call
                    .get("args")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                let id = format!("vertex_tool_{}", result.tool_calls.len());
                result.tool_calls.push(serde_json::json!({
                    "id": id,
                    "name": name,
                    "arguments": args,
                }));
                result.blocks.push(serde_json::json!({
                    "type": "tool_call",
                    "id": id,
                    "name": name,
                    "arguments": args,
                    "visibility": "internal",
                }));
            }
        }
    }
    result.input_tokens = json["usageMetadata"]["promptTokenCount"]
        .as_i64()
        .unwrap_or(0);
    result.output_tokens = json["usageMetadata"]["candidatesTokenCount"]
        .as_i64()
        .unwrap_or(0);
    result.stop_reason = json["candidates"][0]["finishReason"]
        .as_str()
        .map(str::to_string);
    Ok(result)
}

fn service_account_project() -> Result<String, VmError> {
    let path = std::env::var("GOOGLE_APPLICATION_CREDENTIALS")
        .map_err(|_| vm_err("GOOGLE_APPLICATION_CREDENTIALS is not set"))?;
    let key = read_service_account_key(&path)?;
    key.project_id
        .filter(|project| !project.trim().is_empty())
        .ok_or_else(|| vm_err("service account JSON does not contain project_id"))
}

fn read_service_account_key(path: &str) -> Result<ServiceAccountKey, VmError> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| vm_err(format!("failed to read service account JSON: {error}")))?;
    serde_json::from_str(&text)
        .map_err(|error| vm_err(format!("failed to parse service account JSON: {error}")))
}

async fn exchange_service_account_token(path: &str) -> Result<String, VmError> {
    let key = read_service_account_key(path)?;
    let token_uri = key
        .token_uri
        .as_deref()
        .unwrap_or("https://oauth2.googleapis.com/token");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| vm_err(format!("system clock error: {error}")))?
        .as_secs() as usize;
    let claims = ServiceAccountClaims {
        iss: &key.client_email,
        scope: "https://www.googleapis.com/auth/cloud-platform",
        aud: token_uri,
        iat: now,
        exp: now + 3600,
    };
    let jwt = jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
        &claims,
        &jsonwebtoken::EncodingKey::from_rsa_pem(key.private_key.as_bytes())
            .map_err(|error| vm_err(format!("invalid service account private key: {error}")))?,
    )
    .map_err(|error| vm_err(format!("failed to sign service account JWT: {error}")))?;
    let response = crate::llm::shared_utility_client()
        .post(token_uri)
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", jwt.as_str()),
        ])
        .send()
        .await
        .map_err(|error| vm_err(format!("service account token exchange failed: {error}")))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(vm_err(format!(
            "service account token exchange HTTP {status}: {body}"
        )));
    }
    let json: serde_json::Value = response.json().await.map_err(|error| {
        vm_err(format!(
            "service account token response parse error: {error}"
        ))
    })?;
    json["access_token"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| vm_err("service account token response did not include access_token"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::api::{LlmRequestPayload, ThinkingConfig};
    use serde_json::json;

    #[test]
    fn build_request_maps_messages_to_generate_content() {
        let body = VertexProvider::build_request_body(&base_request());
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be brief");
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "hello");
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 32);
    }

    #[test]
    fn parse_response_extracts_text_usage_and_function_calls() {
        let response = json!({
            "candidates": [{
                "content": {"parts": [
                    {"text": "hi"},
                    {"functionCall": {"name": "lookup", "args": {"q": "x"}}}
                ]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {"promptTokenCount": 3, "candidatesTokenCount": 4}
        });
        let result = parse_vertex_response(&response, "gemini-1.5-pro-002").expect("result");
        assert_eq!(result.text, "hi");
        assert_eq!(result.input_tokens, 3);
        assert_eq!(result.output_tokens, 4);
        assert_eq!(result.tool_calls[0]["name"], "lookup");
    }

    fn base_request() -> LlmRequestPayload {
        LlmRequestPayload {
            provider: "vertex".to_string(),
            model: "gemini-1.5-pro-002".to_string(),
            api_key: String::new(),
            fallback_chain: Vec::new(),
            route_fallbacks: Vec::new(),
            messages: vec![json!({"role": "user", "content": "hello"})],
            system: Some("be brief".to_string()),
            max_tokens: 32,
            temperature: Some(0.2),
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
            native_tools: Some(vec![json!({
                "type": "function",
                "function": {
                    "name": "lookup",
                    "description": "Lookup",
                    "parameters": {"type": "object"}
                }
            })]),
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
