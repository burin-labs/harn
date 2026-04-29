//! Amazon Bedrock Runtime provider.
//!
//! Uses Bedrock's Converse API so Claude, Llama, Titan, Mistral, and other
//! Bedrock model IDs share one request shape. Auth is hand-rolled AWS SigV4
//! to avoid pulling the full AWS SDK into the VM crate.

use std::collections::BTreeMap;

use chrono::Utc;
use sha2::{Digest, Sha256};

use crate::llm::api::{DeltaSender, LlmRequestPayload, LlmResult};
use crate::llm::provider::{LlmProvider, LlmProviderChat};
use crate::llm::providers::common::{
    apply_provider_overrides, maybe_emit_delta, percent_encode_path_segment, request_text_content,
    vm_err,
};
use crate::value::VmError;

pub(crate) struct BedrockProvider;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AwsCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SigV4Headers {
    pub authorization: String,
    pub amz_date: String,
    pub content_sha256: String,
    pub security_token: Option<String>,
}

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
            let role = match message.get("role").and_then(|value| value.as_str()) {
                Some("assistant") => "assistant",
                Some("system") => {
                    let text = request_text_content(message);
                    if !text.is_empty() {
                        system.push(serde_json::json!({ "text": text }));
                    }
                    continue;
                }
                _ => "user",
            };
            let text = request_text_content(message);
            if text.is_empty() {
                continue;
            }
            messages.push(serde_json::json!({
                "role": role,
                "content": [{ "text": text }],
            }));
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
        if let Some(tool_config) = bedrock_tool_config(request.native_tools.as_deref()) {
            body["toolConfig"] = tool_config;
        }
        body
    }

    pub(crate) async fn chat_impl(
        &self,
        request: &LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> Result<LlmResult, VmError> {
        let region = resolve_region()?;
        let credentials = resolve_aws_credentials().await?;
        let mut body = Self::build_request_body(request);
        apply_provider_overrides(&mut body, request.provider_overrides.as_ref());
        let body_bytes = serde_json::to_vec(&body)
            .map_err(|error| vm_err(format!("bedrock request serialization failed: {error}")))?;
        let path = format!(
            "/model/{}/converse",
            percent_encode_path_segment(&request.model)
        );
        let base_url = bedrock_base_url(&region);
        let url = format!("{}{}", base_url.trim_end_matches('/'), path);
        let host = host_for_url(&url)?;
        let signed = sign_request(
            &credentials,
            &region,
            "POST",
            &host,
            &path,
            &body_bytes,
            None,
        )?;
        let mut req = crate::llm::shared_blocking_client()
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
        let response = req
            .send()
            .await
            .map_err(|error| vm_err(format!("bedrock API error: {error}")))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(vm_err(format!("bedrock HTTP {status}: {body}")));
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

impl LlmProvider for BedrockProvider {
    fn name(&self) -> &str {
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

fn bedrock_tool_config(tools: Option<&[serde_json::Value]>) -> Option<serde_json::Value> {
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
                result.blocks.push(serde_json::json!({
                    "type": "output_text",
                    "text": text,
                    "visibility": "public",
                }));
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
                result.blocks.push(serde_json::json!({
                    "type": "tool_call",
                    "id": id,
                    "name": name,
                    "arguments": input,
                    "visibility": "internal",
                }));
            }
        }
    }
    result.input_tokens = json["usage"]["inputTokens"].as_i64().unwrap_or(0);
    result.output_tokens = json["usage"]["outputTokens"].as_i64().unwrap_or(0);
    result.stop_reason = json["stopReason"].as_str().map(str::to_string);
    Ok(result)
}

fn bedrock_base_url(region: &str) -> String {
    std::env::var("BEDROCK_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("https://bedrock-runtime.{region}.amazonaws.com"))
}

fn host_for_url(url: &str) -> Result<String, VmError> {
    let parsed =
        url::Url::parse(url).map_err(|error| vm_err(format!("invalid Bedrock URL: {error}")))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| vm_err("Bedrock URL does not contain a host"))?;
    Ok(match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

fn resolve_region() -> Result<String, VmError> {
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
    let home = std::env::var("HOME").ok()?;
    let path = match file_kind {
        "credentials" => format!("{home}/.aws/credentials"),
        "config" => format!("{home}/.aws/config"),
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

fn sign_request(
    credentials: &AwsCredentials,
    region: &str,
    method: &str,
    host: &str,
    path: &str,
    body: &[u8],
    now_override: Option<chrono::DateTime<Utc>>,
) -> Result<SigV4Headers, VmError> {
    let now = now_override.unwrap_or_else(Utc::now);
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = now.format("%Y%m%d").to_string();
    let content_sha256 = sha256_hex(body);
    let mut headers = BTreeMap::from([
        ("content-type".to_string(), "application/json".to_string()),
        ("host".to_string(), host.to_string()),
        ("x-amz-content-sha256".to_string(), content_sha256.clone()),
        ("x-amz-date".to_string(), amz_date.clone()),
    ]);
    if let Some(token) = credentials.session_token.as_ref() {
        headers.insert("x-amz-security-token".to_string(), token.clone());
    }
    let signed_headers = headers.keys().cloned().collect::<Vec<_>>().join(";");
    let canonical_headers = headers
        .iter()
        .map(|(key, value)| format!("{key}:{}\n", value.trim()))
        .collect::<String>();
    let canonical_request =
        format!("{method}\n{path}\n\n{canonical_headers}\n{signed_headers}\n{content_sha256}");
    let credential_scope = format!("{date}/{region}/bedrock/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );
    let date_key = crate::connectors::hmac::hmac_sha256(
        format!("AWS4{}", credentials.secret_access_key).as_bytes(),
        date.as_bytes(),
    );
    let region_key = crate::connectors::hmac::hmac_sha256(&date_key, region.as_bytes());
    let service_key = crate::connectors::hmac::hmac_sha256(&region_key, b"bedrock");
    let signing_key = crate::connectors::hmac::hmac_sha256(&service_key, b"aws4_request");
    let signature = hex::encode(crate::connectors::hmac::hmac_sha256(
        &signing_key,
        string_to_sign.as_bytes(),
    ));
    Ok(SigV4Headers {
        authorization: format!(
            "AWS4-HMAC-SHA256 Credential={}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}",
            credentials.access_key_id
        ),
        amz_date,
        content_sha256,
        security_token: credentials.session_token.clone(),
    })
}

fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(
            body["toolConfig"]["tools"][0]["toolSpec"]["inputSchema"]["json"]["type"],
            "object"
        );
    }

    #[test]
    fn sigv4_signs_bedrock_request_with_session_token() {
        let credentials = AwsCredentials {
            access_key_id: "AKIDEXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
            session_token: Some("session".to_string()),
        };
        let signed = sign_request(
            &credentials,
            "us-east-1",
            "POST",
            "bedrock-runtime.us-east-1.amazonaws.com",
            "/model/anthropic.claude-3-5-sonnet-20240620-v1%3A0/converse",
            br#"{"messages":[]}"#,
            Some(Utc.with_ymd_and_hms(2026, 4, 29, 12, 0, 0).unwrap()),
        )
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
    fn ini_parser_reads_profile_values() {
        let text = r#"
[default]
aws_access_key_id = default-key

[dev]
aws_secret_access_key = dev-secret
"#;
        assert_eq!(
            parse_ini_value(text, "dev", "aws_secret_access_key").as_deref(),
            Some("dev-secret")
        );
    }

    fn base_request() -> LlmRequestPayload {
        LlmRequestPayload {
            provider: "bedrock".to_string(),
            model: "anthropic.claude-3-5-sonnet-20240620-v1:0".to_string(),
            api_key: String::new(),
            fallback_chain: Vec::new(),
            messages: vec![json!({"role": "user", "content": "hello"})],
            system: Some("be brief".to_string()),
            max_tokens: 32,
            temperature: Some(0.1),
            top_p: Some(0.9),
            top_k: None,
            stop: None,
            seed: None,
            frequency_penalty: None,
            presence_penalty: None,
            response_format: None,
            json_schema: None,
            thinking: ThinkingConfig::Disabled,
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
