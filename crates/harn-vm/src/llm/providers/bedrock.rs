//! Amazon Bedrock Runtime provider.
//!
//! Uses Bedrock's Converse API so Claude, Llama, Titan, Mistral, and other
//! Bedrock model IDs share one request shape. Auth is hand-rolled AWS SigV4
//! to avoid pulling the full AWS SDK into the VM crate.

use std::collections::BTreeMap;

use chrono::Utc;

use crate::aws_sigv4::{sign as sign_sigv4_request, AwsSigV4Input};
use crate::llm::api::{DeltaSender, LlmRequestPayload, LlmResult};
use crate::llm::provider::{LlmProvider, LlmProviderChat};
use crate::llm::providers::common::{
    apply_provider_overrides, maybe_emit_delta, request_text_content, vm_err,
};
use crate::llm::providers::schema_compat::{
    sanitize_schema_for_provider, SchemaCompatProfile, SchemaSurface,
};
use crate::url_encoding::percent_encode_component;
use crate::value::VmError;

pub(crate) struct BedrockProvider;

/// Environment variables consulted (in order) when resolving the AWS
/// region for a Bedrock call, before falling back to the AWS profile
/// `config` file. Exposed so the catalog builtins can advertise which
/// knobs a region-aware `.harn` gateway may set, and so a routing-policy
/// chain link's `region` field documents the same vocabulary.
pub(crate) const BEDROCK_REGION_ENV_VARS: &[&str] =
    &["AWS_REGION", "AWS_DEFAULT_REGION", "BEDROCK_REGION"];

/// Best-effort region currently resolved from the environment/profile,
/// for catalog introspection. Returns `None` when nothing is configured
/// (the live call would then require an explicit `region` override or an
/// env var). Never errors — purely informational.
pub(crate) fn current_env_region() -> Option<String> {
    resolve_region(None).ok()
}

pub(crate) use crate::aws_sigv4::AwsSigV4Credentials as AwsCredentials;

impl BedrockProvider {
    pub(crate) fn build_request_body(request: &LlmRequestPayload) -> serde_json::Value {
        let mut messages = Vec::new();
        let mut system = Vec::new();
        if let Some(text) = request.system.as_deref() {
            if !text.is_empty() {
                system.push(serde_json::json!({ "text": text }));
            }
        }
        for message in &request.messages {
            match message.get("role").and_then(|value| value.as_str()) {
                Some("system") => {
                    let text = request_text_content(message);
                    if !text.is_empty() {
                        system.push(serde_json::json!({ "text": text }));
                    }
                }
                Some("tool") | Some("tool_result") => {
                    // A tool-result turn is a `user`-role message carrying a
                    // `toolResult` block. It never has provider-visible text, so
                    // the old text-only path dropped it entirely and broke the
                    // Converse alternation after the first tool call.
                    messages.push(bedrock_tool_result_message(message));
                }
                role => {
                    let role = if role == Some("assistant") {
                        "assistant"
                    } else {
                        "user"
                    };
                    let content = bedrock_content_blocks(message);
                    // Genuinely empty messages (no text, no tool blocks) are
                    // still skipped; a message that carries only tool_use/tool
                    // history is not, because `content` now includes those
                    // blocks.
                    if !content.is_empty() {
                        messages.push(serde_json::json!({
                            "role": role,
                            "content": content,
                        }));
                    }
                }
            }
        }
        if let Some(prefill) = request.prefill.as_deref() {
            if !prefill.is_empty() {
                messages.push(serde_json::json!({
                    "role": "assistant",
                    "content": [{ "text": prefill }],
                }));
            }
        }
        let mut body = serde_json::json!({ "messages": messages });
        if !system.is_empty() {
            body["system"] = serde_json::json!(system);
        }
        let mut inference = serde_json::Map::new();
        if request.max_tokens > 0 {
            inference.insert(
                "maxTokens".to_string(),
                serde_json::json!(request.max_tokens),
            );
        }
        if let Some(temp) = request.temperature {
            inference.insert("temperature".to_string(), serde_json::json!(temp));
        }
        if let Some(top_p) = request.top_p {
            inference.insert("topP".to_string(), serde_json::json!(top_p));
        }
        if let Some(stop) = request.stop.as_ref() {
            inference.insert("stopSequences".to_string(), serde_json::json!(stop));
        }
        if !inference.is_empty() {
            body["inferenceConfig"] = serde_json::Value::Object(inference);
        }
        strip_anthropic_sampling_params(&mut body, request);
        if let Some(tool_config) = bedrock_tool_config(
            &request.provider,
            &request.model,
            request.native_tools.as_deref(),
        ) {
            body["toolConfig"] = tool_config;
        }
        body
    }

    pub(crate) async fn chat_impl(
        &self,
        request: &LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> Result<LlmResult, VmError> {
        let region = resolve_region(request.region.as_deref())?;
        let credentials = resolve_aws_credentials().await?;
        let mut body = Self::build_request_body(request);
        apply_provider_overrides(&mut body, request.provider_overrides.as_ref());
        strip_anthropic_sampling_params(&mut body, request);
        let body_bytes = serde_json::to_vec(&body)
            .map_err(|error| vm_err(format!("bedrock request serialization failed: {error}")))?;
        let path = format!(
            "/model/{}/converse",
            percent_encode_component(&request.model)
        );
        let base_url = bedrock_base_url(&region);
        let url = format!("{}{}", base_url.trim_end_matches('/'), path);
        let sign_headers =
            BTreeMap::from([("Content-Type".to_string(), "application/json".to_string())]);
        let signed = sign_sigv4_request(AwsSigV4Input {
            credentials: &credentials,
            method: "POST",
            url: &url,
            service: "bedrock",
            region: &region,
            headers: &sign_headers,
            body: &body_bytes,
            timestamp: Utc::now(),
        })
        .map_err(|error| vm_err(format!("bedrock request signing failed: {error}")))?;
        let mut req = crate::llm::blocking_client_for_base_url(&base_url)
            .post(url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("X-Amz-Date", signed.amz_date)
            .header("X-Amz-Content-Sha256", signed.content_sha256)
            .header("Authorization", signed.authorization)
            .timeout(std::time::Duration::from_secs(request.resolve_timeout()))
            .body(body_bytes);
        if let Some(token) = signed.security_token {
            req = req.header("X-Amz-Security-Token", token);
        }
        let response = req.send().await.map_err(|error| {
            vm_err(format!(
                "bedrock API error: {}",
                crate::egress::redact_reqwest_error(&error)
            ))
        })?;
        if !response.status().is_success() {
            return Err(crate::llm::api::err_for_non_success("bedrock", response).await);
        }
        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|error| vm_err(format!("bedrock response parse error: {error}")))?;
        let result = parse_bedrock_converse_response(&json, &request.model)?;
        maybe_emit_delta(delta_tx, &result.text);
        Ok(result)
    }
}

fn strip_anthropic_sampling_params(body: &mut serde_json::Value, request: &LlmRequestPayload) {
    crate::llm::providers::anthropic::strip_unsupported_bedrock_converse_sampling_params(
        body,
        &request.model,
        &request.thinking,
    );
}

impl LlmProvider for BedrockProvider {
    fn name(&self) -> &'static str {
        "bedrock"
    }
}

impl LlmProviderChat for BedrockProvider {
    fn chat<'a>(
        &'a self,
        request: &'a LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<LlmResult, VmError>> + 'a>> {
        Box::pin(self.chat_impl(request, delta_tx))
    }
}

/// Build the Converse `content` block list for a `user`/`assistant` message.
///
/// Emits a text block for the message's provider-visible text, then a `toolUse`
/// block per tool call. Tool calls arrive in two internal dialects and both are
/// covered: Anthropic-native `tool_use` blocks inlined in `content` (how a
/// Claude-on-Bedrock assistant turn is recorded), and the OpenAI/Ollama-style
/// top-level `tool_calls` array (how a non-Claude Bedrock model records its
/// calls). Blocks the adapter can't represent (e.g. images) degrade to the old
/// text-flattening behavior — the block is dropped, but the rest of the message
/// (text + tool history) is preserved rather than the whole message vanishing.
fn bedrock_content_blocks(message: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut blocks = Vec::new();
    let content = crate::llm::content::provider_visible_content(&message["content"]);
    match &content {
        serde_json::Value::String(text) => {
            if !text.is_empty() {
                blocks.push(serde_json::json!({ "text": text }));
            }
        }
        serde_json::Value::Array(parts) => {
            for part in parts {
                if let Some(block) = bedrock_content_block_from_part(part) {
                    blocks.push(block);
                }
            }
        }
        serde_json::Value::Null => {}
        other => blocks.push(serde_json::json!({ "text": other.to_string() })),
    }
    if let Some(calls) = message.get("tool_calls").and_then(|value| value.as_array()) {
        for call in calls {
            if let Some(block) = bedrock_tool_use_block_from_call(call) {
                blocks.push(block);
            }
        }
    }
    blocks
}

/// Map one content-array part to a Converse block. Text parts become `text`
/// blocks; inline Anthropic `tool_use` blocks become `toolUse` blocks. Anything
/// else (images, audio) is not representable in this text+tools-only adapter and
/// returns `None` so the caller drops just that block.
fn bedrock_content_block_from_part(part: &serde_json::Value) -> Option<serde_json::Value> {
    if part.get("type").and_then(|value| value.as_str()) == Some("tool_use") {
        return bedrock_tool_use_block(
            part.get("id"),
            part.get("name"),
            part.get("input").cloned(),
        );
    }
    if let Some(text) = part.get("text").and_then(|value| value.as_str()) {
        return Some(serde_json::json!({ "text": text }));
    }
    None
}

/// Map an OpenAI/Ollama-style tool call (`{ id, function: { name, arguments } }`,
/// with a top-level `name`/`arguments` fallback) to a Converse `toolUse` block.
/// The OpenAI `arguments` field is a JSON *string*; Converse `input` is a JSON
/// *object*, so it is parsed (tolerating an already-object form, falling back to
/// an empty object on non-JSON). Returns `None` when no tool name is present.
fn bedrock_tool_use_block_from_call(call: &serde_json::Value) -> Option<serde_json::Value> {
    let function = call.get("function").unwrap_or(call);
    let input = match function.get("arguments") {
        Some(serde_json::Value::String(raw)) => {
            serde_json::from_str::<serde_json::Value>(raw).unwrap_or_else(|_| serde_json::json!({}))
        }
        Some(other) if other.is_object() => other.clone(),
        _ => call
            .get("arguments")
            .filter(|value| value.is_object())
            .cloned()
            .unwrap_or_else(|| serde_json::json!({})),
    };
    bedrock_tool_use_block(
        call.get("id").or_else(|| function.get("id")),
        function.get("name").or_else(|| call.get("name")),
        Some(input),
    )
}

/// Assemble a Converse `toolUse` block from an id/name/input triple, defaulting a
/// missing id to the empty string and a missing/non-object input to `{}`.
/// Returns `None` when the name is missing or empty (a call with no name can't be
/// represented and would be rejected by Bedrock).
fn bedrock_tool_use_block(
    id: Option<&serde_json::Value>,
    name: Option<&serde_json::Value>,
    input: Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    let name = name
        .and_then(|value| value.as_str())
        .filter(|name| !name.is_empty())?;
    let id = id.and_then(|value| value.as_str()).unwrap_or("");
    let input = input
        .filter(|value| value.is_object())
        .unwrap_or_else(|| serde_json::json!({}));
    Some(serde_json::json!({
        "toolUse": {
            "toolUseId": id,
            "name": name,
            "input": input,
        }
    }))
}

/// Build the `user`-role message that carries a tool result as a Converse
/// `toolResult` block. The internal tool-result id lives in `tool_call_id`
/// (OpenAI dialect) or `tool_use_id` (Anthropic dialect); the result payload is
/// the message `content`. A `status` is emitted only when the internal shape
/// distinguishes an error via `is_error`, so ordinary results stay minimal.
fn bedrock_tool_result_message(message: &serde_json::Value) -> serde_json::Value {
    let tool_use_id = message
        .get("tool_call_id")
        .or_else(|| message.get("tool_use_id"))
        .or_else(|| message.get("call_id"))
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let mut tool_result = serde_json::json!({
        "toolUseId": tool_use_id,
        "content": bedrock_tool_result_content(&message["content"]),
    });
    if let Some(is_error) = message.get("is_error").and_then(|value| value.as_bool()) {
        tool_result["status"] = serde_json::json!(if is_error { "error" } else { "success" });
    }
    serde_json::json!({
        "role": "user",
        "content": [{ "toolResult": tool_result }],
    })
}

/// Build the Converse `toolResult.content` list from a tool-result payload. A
/// plain string becomes a `text` block, a JSON object becomes a `json` block,
/// and an array is walked into `text` blocks (non-text parts, e.g. images,
/// degrade away). The list is always non-empty because Converse rejects an empty
/// `toolResult.content`.
fn bedrock_tool_result_content(content: &serde_json::Value) -> Vec<serde_json::Value> {
    let visible = crate::llm::content::provider_visible_content(content);
    let blocks = match &visible {
        serde_json::Value::String(text) => vec![serde_json::json!({ "text": text })],
        serde_json::Value::Object(_) => vec![serde_json::json!({ "json": visible.clone() })],
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(|value| value.as_str())
                    .map(|text| serde_json::json!({ "text": text }))
            })
            .collect(),
        serde_json::Value::Null => Vec::new(),
        other => vec![serde_json::json!({ "text": other.to_string() })],
    };
    if blocks.is_empty() {
        vec![serde_json::json!({ "text": "" })]
    } else {
        blocks
    }
}

fn bedrock_tool_config(
    provider: &str,
    model: &str,
    tools: Option<&[serde_json::Value]>,
) -> Option<serde_json::Value> {
    let mut specs = Vec::new();
    for tool in tools.unwrap_or_default() {
        let function = tool.get("function").unwrap_or(tool);
        let Some(name) = function.get("name").and_then(|value| value.as_str()) else {
            continue;
        };
        let mut spec = serde_json::json!({ "name": name });
        if let Some(description) = function.get("description") {
            spec["description"] = description.clone();
        }
        if let Some(schema) = function
            .get("parameters")
            .or_else(|| function.get("input_schema"))
        {
            let schema = sanitize_schema_for_provider(
                provider,
                model,
                SchemaCompatProfile::AnthropicStrict,
                SchemaSurface::ToolParameters,
                schema,
            );
            spec["inputSchema"] = serde_json::json!({ "json": schema });
        }
        specs.push(serde_json::json!({ "toolSpec": spec }));
    }
    (!specs.is_empty()).then(|| serde_json::json!({ "tools": specs }))
}

fn parse_bedrock_converse_response(
    json: &serde_json::Value,
    model: &str,
) -> Result<LlmResult, VmError> {
    if let Some(message) = json["message"].as_str() {
        return Err(vm_err(format!("bedrock API error: {message}")));
    }
    if let Some(message) = json["error"]["message"].as_str() {
        return Err(vm_err(format!("bedrock API error: {message}")));
    }
    let mut result = crate::llm::providers::common::empty_result("bedrock", model);
    if let Some(content) = json["output"]["message"]["content"].as_array() {
        for block in content {
            if let Some(text) = block.get("text").and_then(|value| value.as_str()) {
                result.text.push_str(text);
                result
                    .blocks
                    .push(crate::llm::providers::common::output_text_block(text));
            }
            if let Some(tool_use) = block.get("toolUse") {
                let id = tool_use
                    .get("toolUseId")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = tool_use
                    .get("name")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                let input = tool_use
                    .get("input")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                result.tool_calls.push(serde_json::json!({
                    "id": id,
                    "name": name,
                    "arguments": input,
                }));
                result
                    .blocks
                    .push(crate::llm::providers::common::tool_call_block(
                        id, name, input,
                    ));
            }
        }
    }
    result.input_tokens = json["usage"]["inputTokens"].as_i64().unwrap_or(0);
    result.output_tokens = json["usage"]["outputTokens"].as_i64().unwrap_or(0);
    result.cache_read_tokens = json["usage"]["cacheReadInputTokens"].as_i64().unwrap_or(0);
    result.cache_write_tokens = json["usage"]["cacheWriteInputTokens"].as_i64().unwrap_or(0);
    result.stop_reason = json["stopReason"].as_str().map(str::to_string);
    Ok(result)
}

fn bedrock_base_url(region: &str) -> String {
    std::env::var("BEDROCK_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("https://bedrock-runtime.{region}.amazonaws.com"))
}

/// Resolve the AWS region for a Bedrock call.
///
/// An explicit per-call override (from a routing-policy chain link's
/// `region` field, threaded through `LlmRequestPayload::region`) wins
/// over every environment/profile source. When the override is `None`
/// or blank, resolution falls back to the historical env/profile chain,
/// so existing scripts that never set a region are unaffected.
fn resolve_region(override_region: Option<&str>) -> Result<String, VmError> {
    if let Some(region) = override_region {
        let trimmed = region.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    for env_name in ["AWS_REGION", "AWS_DEFAULT_REGION", "BEDROCK_REGION"] {
        if let Ok(region) = std::env::var(env_name) {
            if !region.trim().is_empty() {
                return Ok(region);
            }
        }
    }
    let profile = std::env::var("AWS_PROFILE").unwrap_or_else(|_| "default".to_string());
    if let Some(region) = read_aws_profile_value("config", &profile, "region") {
        return Ok(region);
    }
    Err(vm_err(
        "AWS region is not configured; set AWS_REGION, AWS_DEFAULT_REGION, or BEDROCK_REGION",
    ))
}

async fn resolve_aws_credentials() -> Result<AwsCredentials, VmError> {
    if let (Ok(access_key_id), Ok(secret_access_key)) = (
        std::env::var("AWS_ACCESS_KEY_ID"),
        std::env::var("AWS_SECRET_ACCESS_KEY"),
    ) {
        if !access_key_id.trim().is_empty() && !secret_access_key.trim().is_empty() {
            return Ok(AwsCredentials {
                access_key_id,
                secret_access_key,
                session_token: std::env::var("AWS_SESSION_TOKEN").ok(),
            });
        }
    }
    let profile = std::env::var("AWS_PROFILE").unwrap_or_else(|_| "default".to_string());
    if let (Some(access_key_id), Some(secret_access_key)) = (
        read_aws_profile_value("credentials", &profile, "aws_access_key_id"),
        read_aws_profile_value("credentials", &profile, "aws_secret_access_key"),
    ) {
        return Ok(AwsCredentials {
            access_key_id,
            secret_access_key,
            session_token: read_aws_profile_value("credentials", &profile, "aws_session_token"),
        });
    }
    if let Some(credentials) = resolve_container_credentials().await? {
        return Ok(credentials);
    }
    if let Some(credentials) = resolve_instance_profile_credentials().await? {
        return Ok(credentials);
    }
    Err(vm_err(
        "AWS credentials not found: set AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY, configure an AWS profile, or run on an instance/container role",
    ))
}

fn read_aws_profile_value(file_kind: &str, profile: &str, key: &str) -> Option<String> {
    let home = crate::user_dirs::home_dir()?;
    let path = match file_kind {
        "credentials" => home.join(".aws").join("credentials"),
        "config" => home.join(".aws").join("config"),
        _ => return None,
    };
    let text = std::fs::read_to_string(path).ok()?;
    let profile_section = if file_kind == "config" && profile != "default" {
        format!("profile {profile}")
    } else {
        profile.to_string()
    };
    parse_ini_value(&text, &profile_section, key)
}

fn parse_ini_value(text: &str, section: &str, key: &str) -> Option<String> {
    let mut in_section = false;
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_section = &line[1..line.len() - 1] == section;
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((candidate, value)) = line.split_once('=') else {
            continue;
        };
        if candidate.trim() == key {
            return Some(value.trim().to_string());
        }
    }
    None
}

async fn resolve_container_credentials() -> Result<Option<AwsCredentials>, VmError> {
    let url = if let Ok(full) = std::env::var("AWS_CONTAINER_CREDENTIALS_FULL_URI") {
        full
    } else if let Ok(relative) = std::env::var("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI") {
        format!("http://169.254.170.2{relative}")
    } else {
        return Ok(None);
    };
    let mut req = crate::llm::shared_utility_client()
        .get(url)
        .timeout(std::time::Duration::from_secs(2));
    if let Ok(token) = std::env::var("AWS_CONTAINER_AUTHORIZATION_TOKEN") {
        req = req.header("Authorization", token);
    }
    let response = match req.send().await {
        Ok(response) => response,
        Err(_) => return Ok(None),
    };
    if !response.status().is_success() {
        return Ok(None);
    }
    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|error| vm_err(format!("container credential parse error: {error}")))?;
    Ok(credentials_from_metadata_json(&json))
}

async fn resolve_instance_profile_credentials() -> Result<Option<AwsCredentials>, VmError> {
    let client = crate::llm::shared_utility_client();
    let token = match client
        .put("http://169.254.169.254/latest/api/token")
        .header("X-aws-ec2-metadata-token-ttl-seconds", "21600")
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => response.text().await.ok(),
        _ => None,
    };
    let mut role_req = client
        .get("http://169.254.169.254/latest/meta-data/iam/security-credentials/")
        .timeout(std::time::Duration::from_secs(2));
    if let Some(token) = token.as_deref() {
        role_req = role_req.header("X-aws-ec2-metadata-token", token);
    }
    let role = match role_req.send().await {
        Ok(response) if response.status().is_success() => response.text().await.ok(),
        _ => None,
    };
    let Some(role) = role
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let mut cred_req = client
        .get(format!(
            "http://169.254.169.254/latest/meta-data/iam/security-credentials/{role}"
        ))
        .timeout(std::time::Duration::from_secs(2));
    if let Some(token) = token.as_deref() {
        cred_req = cred_req.header("X-aws-ec2-metadata-token", token);
    }
    let response = match cred_req.send().await {
        Ok(response) if response.status().is_success() => response,
        _ => return Ok(None),
    };
    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|error| vm_err(format!("instance profile credential parse error: {error}")))?;
    Ok(credentials_from_metadata_json(&json))
}

fn credentials_from_metadata_json(json: &serde_json::Value) -> Option<AwsCredentials> {
    Some(AwsCredentials {
        access_key_id: json
            .get("AccessKeyId")
            .or_else(|| json.get("AccessKeyID"))?
            .as_str()?
            .to_string(),
        secret_access_key: json.get("SecretAccessKey")?.as_str()?.to_string(),
        session_token: json
            .get("Token")
            .and_then(|value| value.as_str())
            .map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aws_sigv4::AwsSigV4Input;
    use crate::llm::api::{LlmRequestPayload, ThinkingConfig};
    use chrono::TimeZone;
    use serde_json::json;

    #[test]
    fn converse_body_maps_messages_system_inference_and_tools() {
        let body = BedrockProvider::build_request_body(&base_request());
        assert_eq!(body["system"][0]["text"], "be brief");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"][0]["text"], "hello");
        assert_eq!(body["inferenceConfig"]["maxTokens"], 32);
        assert_eq!(body["inferenceConfig"]["temperature"], json!(0.1));
        assert_eq!(body["inferenceConfig"]["topP"], json!(0.9));
        assert_eq!(
            body["toolConfig"]["tools"][0]["toolSpec"]["inputSchema"]["json"]["type"],
            "object"
        );
        let schema = &body["toolConfig"]["tools"][0]["toolSpec"]["inputSchema"]["json"];
        assert_eq!(schema["additionalProperties"], false);
        assert!(schema["properties"]["query"].get("pattern").is_none());
        assert!(schema["properties"]["query"].get("default").is_none());
    }

    #[test]
    fn converse_body_round_trips_assistant_tool_use_and_tool_result() {
        // Anthropic-native dialect (Claude on Bedrock): the assistant turn
        // records its call as an inline `tool_use` content block, and the result
        // comes back as a top-level `role:"tool_result"` message. Before the fix
        // both were dropped (no provider-visible text), producing consecutive
        // user turns that Bedrock 400s on.
        let mut request = base_request();
        request.messages = vec![
            json!({"role": "user", "content": "look it up"}),
            json!({"role": "assistant", "content": [
                {"type": "text", "text": "on it"},
                {"type": "tool_use", "id": "t1", "name": "lookup", "input": {"q": "x"}}
            ]}),
            json!({"role": "tool_result", "tool_use_id": "t1", "content": "found it"}),
        ];

        let body = BedrockProvider::build_request_body(&request);
        let messages = body["messages"].as_array().expect("messages");

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"][0]["text"], "on it");
        assert_eq!(messages[1]["content"][1]["toolUse"]["toolUseId"], "t1");
        assert_eq!(messages[1]["content"][1]["toolUse"]["name"], "lookup");
        assert_eq!(messages[1]["content"][1]["toolUse"]["input"]["q"], "x");
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"][0]["toolResult"]["toolUseId"], "t1");
        assert_eq!(
            messages[2]["content"][0]["toolResult"]["content"][0]["text"],
            "found it"
        );
        // Roles alternate user/assistant/user — no consecutive same-role turns.
        assert_ne!(messages[0]["role"], messages[1]["role"]);
        assert_ne!(messages[1]["role"], messages[2]["role"]);
    }

    #[test]
    fn converse_body_maps_openai_dialect_tool_calls_and_tool_role() {
        // Non-Claude Bedrock model dialect: assistant carries a top-level
        // `tool_calls` array (arguments as a JSON string) and the result is a
        // top-level `role:"tool"` message keyed by `tool_call_id`.
        let mut request = base_request();
        request.model = "meta.llama3-70b-instruct-v1:0".to_string();
        request.messages = vec![
            json!({"role": "user", "content": "look it up"}),
            json!({"role": "assistant", "content": "", "tool_calls": [
                {"id": "call_1", "type": "function",
                 "function": {"name": "lookup", "arguments": "{\"q\":\"x\"}"}}
            ]}),
            json!({"role": "tool", "tool_call_id": "call_1", "name": "lookup",
                   "content": "boom", "is_error": true}),
        ];

        let body = BedrockProvider::build_request_body(&request);
        let messages = body["messages"].as_array().expect("messages");

        assert_eq!(messages.len(), 3);
        // Empty assistant text yields a tool_use-only content list (no stray
        // empty text block).
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"].as_array().map(Vec::len), Some(1));
        assert_eq!(messages[1]["content"][0]["toolUse"]["toolUseId"], "call_1");
        assert_eq!(messages[1]["content"][0]["toolUse"]["name"], "lookup");
        assert_eq!(messages[1]["content"][0]["toolUse"]["input"]["q"], "x");
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(
            messages[2]["content"][0]["toolResult"]["toolUseId"],
            "call_1"
        );
        assert_eq!(
            messages[2]["content"][0]["toolResult"]["content"][0]["text"],
            "boom"
        );
        assert_eq!(messages[2]["content"][0]["toolResult"]["status"], "error");
    }

    #[test]
    fn converse_body_preserves_plain_text_conversation() {
        let mut request = base_request();
        request.messages = vec![
            json!({"role": "user", "content": "hi"}),
            json!({"role": "assistant", "content": "hello there"}),
            json!({"role": "user", "content": "thanks"}),
        ];

        let body = BedrockProvider::build_request_body(&request);
        let messages = body["messages"].as_array().expect("messages");

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"][0]["text"], "hi");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"][0]["text"], "hello there");
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"][0]["text"], "thanks");
        // No stray toolUse/toolResult blocks in a plain conversation.
        assert!(messages[1]["content"][0].get("toolUse").is_none());
    }

    #[test]
    fn converse_body_emits_both_text_and_tool_use_for_mixed_assistant() {
        let mut request = base_request();
        request.messages = vec![json!({
            "role": "assistant",
            "content": "let me check",
            "tool_calls": [
                {"id": "call_1", "type": "function",
                 "function": {"name": "lookup", "arguments": "{\"q\":\"x\"}"}}
            ]
        })];

        let body = BedrockProvider::build_request_body(&request);
        let content = body["messages"][0]["content"].as_array().expect("content");

        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["text"], "let me check");
        assert_eq!(content[1]["toolUse"]["name"], "lookup");
        assert_eq!(content[1]["toolUse"]["input"]["q"], "x");
    }

    #[test]
    fn converse_body_skips_genuinely_empty_messages() {
        let mut request = base_request();
        request.messages = vec![
            json!({"role": "user", "content": "hi"}),
            json!({"role": "assistant", "content": ""}),
        ];

        let body = BedrockProvider::build_request_body(&request);
        let messages = body["messages"].as_array().expect("messages");

        // The empty assistant message (no text, no tool blocks) is dropped.
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["content"][0]["text"], "hi");
    }

    #[test]
    fn claude_47_converse_body_strips_sampling_params() {
        let mut request = base_request();
        request.model = "anthropic.claude-opus-4-7-v1:0".to_string();

        let body = BedrockProvider::build_request_body(&request);

        assert_eq!(body["inferenceConfig"]["maxTokens"], 32);
        assert!(body["inferenceConfig"].get("temperature").is_none());
        assert!(body["inferenceConfig"].get("topP").is_none());
    }

    #[test]
    fn non_claude_converse_body_preserves_sampling_params() {
        let mut request = base_request();
        request.model = "meta.llama3-70b-instruct-v1:0".to_string();
        request.thinking = ThinkingConfig::Enabled {
            budget_tokens: Some(1024),
        };

        let body = BedrockProvider::build_request_body(&request);

        assert_eq!(body["inferenceConfig"]["temperature"], json!(0.1));
        assert_eq!(body["inferenceConfig"]["topP"], json!(0.9));
    }

    #[test]
    fn claude_converse_override_thinking_strips_reinserted_sampling_params() {
        let mut request = base_request();
        request.model = "anthropic.claude-sonnet-4-6-v1:0".to_string();
        request.temperature = None;
        request.top_p = None;
        request.provider_overrides = Some(json!({
            "inferenceConfig": {
                "maxTokens": 32,
                "temperature": 0.0,
                "topP": 0.9,
                "topK": 20
            },
            "additionalModelRequestFields": {
                "thinking": {"type": "enabled", "budget_tokens": 1024}
            }
        }));

        let mut body = BedrockProvider::build_request_body(&request);
        apply_provider_overrides(&mut body, request.provider_overrides.as_ref());
        strip_anthropic_sampling_params(&mut body, &request);

        assert_eq!(body["inferenceConfig"]["maxTokens"], 32);
        assert!(body["inferenceConfig"].get("temperature").is_none());
        assert!(body["inferenceConfig"].get("topP").is_none());
        assert!(body["inferenceConfig"].get("topK").is_none());
        assert_eq!(
            body["additionalModelRequestFields"]["thinking"],
            json!({"type": "enabled", "budget_tokens": 1024})
        );
    }

    #[test]
    fn sigv4_signs_bedrock_request_with_session_token() {
        let credentials = AwsCredentials {
            access_key_id: "AKIDEXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
            session_token: Some("session".to_string()),
        };
        let headers =
            BTreeMap::from([("Content-Type".to_string(), "application/json".to_string())]);
        let signed = sign_sigv4_request(AwsSigV4Input {
            credentials: &credentials,
            method: "POST",
            url: "https://bedrock-runtime.us-east-1.amazonaws.com/model/anthropic.claude-3-5-sonnet-20240620-v1%3A0/converse",
            service: "bedrock",
            region: "us-east-1",
            headers: &headers,
            body: br#"{"messages":[]}"#,
            timestamp: Utc.with_ymd_and_hms(2026, 4, 29, 12, 0, 0).unwrap(),
        })
        .expect("signature");
        assert_eq!(signed.amz_date, "20260429T120000Z");
        assert_eq!(signed.security_token.as_deref(), Some("session"));
        assert!(signed
            .authorization
            .contains("Credential=AKIDEXAMPLE/20260429/us-east-1/bedrock/aws4_request"));
        assert!(signed.authorization.contains(
            "SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date;x-amz-security-token"
        ));
    }

    #[test]
    fn parse_converse_response_extracts_text_tools_and_usage() {
        let json = json!({
            "output": {"message": {"content": [
                {"text": "hello"},
                {"toolUse": {"toolUseId": "t1", "name": "lookup", "input": {"q": "x"}}}
            ]}},
            "usage": {"inputTokens": 2, "outputTokens": 3},
            "stopReason": "tool_use"
        });
        let result = parse_bedrock_converse_response(&json, "meta.llama3-70b-instruct-v1:0")
            .expect("result");
        assert_eq!(result.text, "hello");
        assert_eq!(result.input_tokens, 2);
        assert_eq!(result.output_tokens, 3);
        assert_eq!(result.tool_calls[0]["name"], "lookup");
    }

    #[test]
    fn parse_converse_response_surfaces_prompt_cache_tokens() {
        let json = json!({
            "output": {"message": {"content": [{"text": "hi"}]}},
            "usage": {
                "inputTokens": 5,
                "outputTokens": 7,
                "cacheReadInputTokens": 11,
                "cacheWriteInputTokens": 13
            },
            "stopReason": "end_turn"
        });
        let result = parse_bedrock_converse_response(&json, "anthropic.claude-3-5-sonnet-v2:0")
            .expect("result");
        assert_eq!(result.input_tokens, 5);
        assert_eq!(result.output_tokens, 7);
        assert_eq!(result.cache_read_tokens, 11);
        assert_eq!(result.cache_write_tokens, 13);
    }

    #[test]
    fn explicit_region_override_wins_over_env() {
        // The override path returns before touching the environment, so
        // this is deterministic regardless of ambient AWS_* vars.
        assert_eq!(
            resolve_region(Some("eu-west-1")).expect("override region"),
            "eu-west-1"
        );
        // Surrounding whitespace is trimmed.
        assert_eq!(
            resolve_region(Some("  ap-southeast-2  ")).expect("trimmed region"),
            "ap-southeast-2"
        );
    }

    #[test]
    fn blank_region_override_falls_back_to_env() {
        let _guard = crate::llm::env_guard();
        let saved: Vec<(&str, Option<String>)> = BEDROCK_REGION_ENV_VARS
            .iter()
            .map(|name| (*name, std::env::var(name).ok()))
            .collect();
        for name in BEDROCK_REGION_ENV_VARS {
            std::env::remove_var(name);
        }
        std::env::set_var("AWS_REGION", "us-east-2");

        // A `None` override falls through to the env chain...
        assert_eq!(resolve_region(None).expect("env region"), "us-east-2");
        // ...as does a blank/whitespace override (so an empty chain-link
        // `region: ""` doesn't accidentally pin an invalid region).
        assert_eq!(
            resolve_region(Some("   ")).expect("env region"),
            "us-east-2"
        );

        for (name, value) in saved {
            match value {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        }
    }

    #[test]
    fn ini_parser_reads_profile_values() {
        let text = r"
[default]
aws_access_key_id = default-key

[dev]
aws_secret_access_key = dev-secret
";
        assert_eq!(
            parse_ini_value(text, "dev", "aws_secret_access_key").as_deref(),
            Some("dev-secret")
        );
    }

    fn base_request() -> LlmRequestPayload {
        LlmRequestPayload {
            provider: "bedrock".to_string(),
            model: "anthropic.claude-3-5-sonnet-20240620-v1:0".to_string(),
            region: None,
            api_key: String::new(),
            api_mode: crate::llm::api::LlmApiMode::ChatCompletions,
            messages: vec![json!({"role": "user", "content": "hello"})],
            system: Some("be brief".to_string()),
            max_tokens: 32,
            temperature: Some(0.1),
            top_p: Some(0.9),
            top_k: None,
            logprobs: false,
            top_logprobs: None,
            stop: None,
            seed: None,
            frequency_penalty: None,
            presence_penalty: None,
            fast: false,
            output_format: crate::llm::api::OutputFormat::Text,
            response_format: None,
            json_schema: None,
            output_schema: None,
            schema_stream_abort: false,
            thinking: ThinkingConfig::Disabled,
            anthropic_beta_features: Vec::new(),
            vision: false,
            native_tools: Some(vec![json!({
                "type": "function",
                "function": {
                    "name": "lookup",
                    "description": "Lookup",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "query": {
                                "type": "string",
                                "pattern": "^harn",
                                "default": "harn"
                            }
                        }
                    }
                }
            })]),
            provider_tools: Vec::new(),
            tool_choice: None,
            cache: false,
            prompt_cache_ttl: None,
            timeout: None,
            stream: false,
            provider_overrides: None,
            previous_response_id: None,
            store: None,
            background: None,
            truncation: None,
            compact: None,
            include: None,
            max_tool_calls: None,
            prefill: None,
            session_id: None,
            reminder_lifecycle: Vec::new(),
            cli_llm_mock_scope: None,
        }
    }
}
