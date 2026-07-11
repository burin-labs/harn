//! Agent Client Protocol provider adapter.
//!
//! This lets a configured provider launch an ACP agent over stdio and drive one
//! prompt turn through `initialize` -> `session/new` -> `session/prompt`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value as JsonValue};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::llm::api::{DeltaSender, LlmRequestPayload, LlmResult, ProviderTelemetry};
use crate::llm::provider::{LlmProvider, LlmProviderChat};
use crate::value::VmError;

use super::common::vm_err;

pub(crate) struct AcpProvider {
    provider: String,
}

#[derive(Clone, Debug)]
struct AcpProviderRuntime {
    provider: String,
    command: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    cwd: String,
    mcp_servers: Vec<JsonValue>,
    client_capabilities: JsonValue,
    client_info: JsonValue,
}

struct AcpConnection<R, W> {
    reader: R,
    writer: W,
    next_id: u64,
}

struct PromptCollector {
    text: String,
    delta_tx: Option<DeltaSender>,
}

impl AcpProvider {
    pub(crate) fn new(provider: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
        }
    }

    pub(crate) fn is_configured_acp(provider: &str) -> bool {
        crate::llm_config::provider_uses_acp(provider)
    }

    pub(crate) async fn chat_impl(
        &self,
        request: &LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> Result<LlmResult, VmError> {
        let runtime = AcpProviderRuntime::from_request(request)?;
        run_acp_provider_process(runtime, request, delta_tx).await
    }
}

impl LlmProvider for AcpProvider {
    fn name(&self) -> &str {
        &self.provider
    }

    fn requires_model(&self) -> bool {
        false
    }
}

impl LlmProviderChat for AcpProvider {
    fn chat<'a>(
        &'a self,
        request: &'a LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<LlmResult, VmError>> + 'a>> {
        Box::pin(self.chat_impl(request, delta_tx))
    }
}

impl AcpProviderRuntime {
    fn from_request(request: &LlmRequestPayload) -> Result<Self, VmError> {
        let provider = request.provider.clone();
        let pdef = crate::llm_config::provider_config(&provider).ok_or_else(|| {
            vm_err(format!(
                "ACP provider `{provider}` is not declared in providers.toml or [llm.providers]"
            ))
        })?;
        let overrides = request.provider_overrides.as_ref();
        let command = override_string(overrides, "command")
            .or_else(|| pdef.command.clone())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                vm_err(format!(
                    "ACP provider `{provider}` requires `command = \"...\"`"
                ))
            })?;
        let args = override_string_array(overrides, "args").unwrap_or(pdef.args.clone());
        let mut env = pdef.env.clone();
        if let Some(extra_env) = override_string_map(overrides, "env")? {
            env.extend(extra_env);
        }
        let raw_cwd = override_string(overrides, "cwd").or(pdef.cwd.clone());
        let cwd = absolute_cwd(raw_cwd)?;
        let mcp_servers = override_json_array(overrides, "mcpServers")
            .or_else(|| override_json_array(overrides, "mcp_servers"))
            .unwrap_or(pdef.mcp_servers);
        let client_capabilities =
            override_json_object(overrides, "clientCapabilities").unwrap_or_else(|| json!({}));
        let client_info = override_json_object(overrides, "clientInfo").unwrap_or_else(|| {
            json!({
                "name": "harn",
                "title": "Harn ACP provider adapter",
                "version": env!("CARGO_PKG_VERSION"),
            })
        });

        Ok(Self {
            provider,
            command,
            args,
            env,
            cwd,
            mcp_servers,
            client_capabilities,
            client_info,
        })
    }
}

async fn run_acp_provider_process(
    runtime: AcpProviderRuntime,
    request: &LlmRequestPayload,
    delta_tx: Option<DeltaSender>,
) -> Result<LlmResult, VmError> {
    let mut command = Command::new(&runtime.command);
    command
        .args(&runtime.args)
        .current_dir(&runtime.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in &runtime.env {
        command.env(key, value);
    }
    let mut child = command.spawn().map_err(|error| {
        vm_err(format!(
            "failed to launch ACP provider `{}` with `{}`: {error}",
            runtime.provider, runtime.command
        ))
    })?;
    let stdin = child.stdin.take().ok_or_else(|| {
        vm_err(format!(
            "ACP provider `{}` did not expose stdin",
            runtime.provider
        ))
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        vm_err(format!(
            "ACP provider `{}` did not expose stdout",
            runtime.provider
        ))
    })?;
    let stderr_task = child.stderr.take().map(|stderr| {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "harn::llm::acp", stderr = %line);
            }
        })
    });

    let timeout = Duration::from_secs(request.resolve_timeout());
    let result = match tokio::time::timeout(
        timeout,
        run_acp_session(BufReader::new(stdout), stdin, runtime, request, delta_tx),
    )
    .await
    {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err(format!(
            "ACP provider `{}` timed out after {}s",
            request.provider,
            timeout.as_secs()
        )),
    };

    let _ = child.kill().await;
    let _ = child.wait().await;
    if let Some(task) = stderr_task {
        task.abort();
    }
    result.map_err(vm_err)
}

async fn run_acp_session<R, W>(
    reader: R,
    writer: W,
    runtime: AcpProviderRuntime,
    request: &LlmRequestPayload,
    delta_tx: Option<DeltaSender>,
) -> Result<LlmResult, VmError>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut connection = AcpConnection {
        reader,
        writer,
        next_id: 1,
    };
    let initialize = connection
        .request(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": runtime.client_capabilities.clone(),
                "clientInfo": runtime.client_info.clone(),
            }),
            None,
        )
        .await?;
    let protocol_version = initialize
        .get("protocolVersion")
        .and_then(JsonValue::as_i64)
        .unwrap_or(1);
    if protocol_version != 1 {
        return Err(vm_err(format!(
            "ACP provider `{}` selected unsupported protocolVersion {protocol_version}",
            runtime.provider
        )));
    }

    let created = connection
        .request(
            "session/new",
            json!({
                "cwd": runtime.cwd.clone(),
                "mcpServers": runtime.mcp_servers.clone(),
            }),
            None,
        )
        .await?;
    let session_id = created
        .get("sessionId")
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            vm_err(format!(
                "ACP provider `{}` returned session/new without sessionId",
                runtime.provider
            ))
        })?
        .to_string();

    let prompt_text = request_prompt_text(request);
    let mut collector = PromptCollector {
        text: String::new(),
        delta_tx,
    };
    let prompt_result = connection
        .request(
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": prompt_text.clone()}],
                "_meta": {
                    "harn": {
                        "provider": request.provider.clone(),
                        "model": request.model.clone(),
                    }
                }
            }),
            Some(&mut collector),
        )
        .await?;
    let stop_reason = prompt_result
        .get("stopReason")
        .and_then(JsonValue::as_str)
        .map(str::to_string);
    let output_tokens = approximate_tokens(&collector.text);
    Ok(LlmResult {
        served_fast: false,
        text: collector.text.clone(),
        tool_calls: Vec::new(),
        input_tokens: approximate_tokens(&prompt_text),
        output_tokens,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cache_supported: true,
        model: request.model.clone(),
        provider: request.provider.clone(),
        thinking: None,
        thinking_summary: None,
        stop_reason,
        blocks: if collector.text.is_empty() {
            Vec::new()
        } else {
            vec![json!({
                "type": "output_text",
                "text": collector.text,
                "visibility": "public",
            })]
        },
        logprobs: Vec::new(),
        telemetry: ProviderTelemetry::default(),
    })
}

impl<R, W> AcpConnection<R, W>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    async fn request(
        &mut self,
        method: &str,
        params: JsonValue,
        mut collector: Option<&mut PromptCollector>,
    ) -> Result<JsonValue, VmError> {
        let id = self.next_id;
        self.next_id += 1;
        self.send_json(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .await?;

        loop {
            let message = self.recv_json().await?;
            if message.get("id") == Some(&json!(id)) && message.get("method").is_none() {
                if let Some(error) = message.get("error") {
                    return Err(vm_err(format!(
                        "ACP provider {method} failed: {}",
                        jsonrpc_error_message(error)
                    )));
                }
                return Ok(message.get("result").cloned().unwrap_or(JsonValue::Null));
            }
            if message.get("id").is_some() && message.get("method").is_some() {
                self.respond_to_client_request(&message).await?;
                continue;
            }
            if let Some(collector) = collector.as_deref_mut() {
                collector.observe(&message);
            }
        }
    }

    async fn send_json(&mut self, message: &JsonValue) -> Result<(), VmError> {
        let line = serde_json::to_string(message)
            .map_err(|error| vm_err(format!("failed to serialize ACP message: {error}")))?;
        self.writer
            .write_all(line.as_bytes())
            .await
            .map_err(|error| vm_err(format!("failed to write ACP message: {error}")))?;
        self.writer
            .write_all(b"\n")
            .await
            .map_err(|error| vm_err(format!("failed to write ACP newline: {error}")))?;
        self.writer
            .flush()
            .await
            .map_err(|error| vm_err(format!("failed to flush ACP message: {error}")))
    }

    async fn recv_json(&mut self) -> Result<JsonValue, VmError> {
        loop {
            let mut line = String::new();
            let read = self
                .reader
                .read_line(&mut line)
                .await
                .map_err(|error| vm_err(format!("failed to read ACP message: {error}")))?;
            if read == 0 {
                return Err(vm_err("ACP provider closed stdout before responding"));
            }
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            return serde_json::from_str(line)
                .map_err(|error| vm_err(format!("ACP provider wrote invalid JSON: {error}")));
        }
    }

    async fn respond_to_client_request(&mut self, message: &JsonValue) -> Result<(), VmError> {
        let id = message.get("id").cloned().unwrap_or(JsonValue::Null);
        let method = message
            .get("method")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        let response = if method == crate::llm::acp_permission::METHOD_REQUEST_PERMISSION {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {"outcome": {"outcome": "cancelled"}},
            })
        } else {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("ACP client method `{method}` is not available through the Harn provider adapter"),
                },
            })
        };
        self.send_json(&response).await
    }
}

impl PromptCollector {
    fn observe(&mut self, message: &JsonValue) {
        if message.get("method").and_then(JsonValue::as_str) != Some("session/update") {
            return;
        }
        let update = &message["params"]["update"];
        if update.get("sessionUpdate").and_then(JsonValue::as_str) != Some("agent_message_chunk") {
            return;
        }
        let text = content_text(&update["content"]);
        if text.is_empty() {
            return;
        }
        if let Some(tx) = &self.delta_tx {
            let _ = tx.send(text.clone());
        }
        self.text.push_str(&text);
    }
}

fn request_prompt_text(request: &LlmRequestPayload) -> String {
    let mut parts = Vec::new();
    if let Some(system) = request.system.as_deref().filter(|value| !value.is_empty()) {
        parts.push(format!("System:\n{system}"));
    }
    if request.system.as_deref().is_none_or(str::is_empty) && request.messages.len() == 1 {
        let message = &request.messages[0];
        if message.get("role").and_then(JsonValue::as_str) == Some("user") {
            let text = request_content_text(&message["content"]);
            if !text.is_empty() {
                return text;
            }
        }
    }
    for message in &request.messages {
        let role = message
            .get("role")
            .and_then(JsonValue::as_str)
            .unwrap_or("message");
        let text = request_content_text(&message["content"]);
        if text.is_empty() {
            continue;
        }
        parts.push(format!("{role}:\n{text}"));
    }
    parts.join("\n\n")
}

fn request_content_text(value: &JsonValue) -> String {
    let visible = crate::llm::content::provider_visible_content(value);
    content_text(&visible)
}

fn content_text(value: &JsonValue) -> String {
    match value {
        JsonValue::String(text) => text.clone(),
        JsonValue::Array(items) => items
            .iter()
            .map(content_text)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        JsonValue::Object(object) => object
            .get("text")
            .and_then(JsonValue::as_str)
            .map(str::to_string)
            .or_else(|| object.get("content").map(content_text))
            .or_else(|| object.get("resource").map(content_text))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn approximate_tokens(text: &str) -> i64 {
    if text.is_empty() {
        0
    } else {
        text.chars().count().div_ceil(4) as i64
    }
}

fn jsonrpc_error_message(error: &JsonValue) -> String {
    error
        .get("message")
        .and_then(JsonValue::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| error.to_string())
}

fn absolute_cwd(raw: Option<String>) -> Result<String, VmError> {
    let path = match raw {
        Some(raw) => PathBuf::from(raw),
        None => std::env::current_dir()
            .map_err(|error| vm_err(format!("failed to resolve ACP provider cwd: {error}")))?,
    };
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|error| vm_err(format!("failed to resolve ACP provider cwd: {error}")))?
            .join(path)
    };
    Ok(absolute.to_string_lossy().to_string())
}

fn override_string(overrides: Option<&JsonValue>, key: &str) -> Option<String> {
    overrides?
        .get(key)
        .and_then(JsonValue::as_str)
        .map(str::to_string)
}

fn override_string_array(overrides: Option<&JsonValue>, key: &str) -> Option<Vec<String>> {
    overrides?.get(key)?.as_array().map(|items| {
        items
            .iter()
            .filter_map(JsonValue::as_str)
            .map(str::to_string)
            .collect()
    })
}

fn override_json_array(overrides: Option<&JsonValue>, key: &str) -> Option<Vec<JsonValue>> {
    overrides?.get(key)?.as_array().cloned()
}

fn override_json_object(overrides: Option<&JsonValue>, key: &str) -> Option<JsonValue> {
    let value = overrides?.get(key)?;
    value.as_object()?;
    Some(value.clone())
}

fn override_string_map(
    overrides: Option<&JsonValue>,
    key: &str,
) -> Result<Option<BTreeMap<String, String>>, VmError> {
    let Some(object) = overrides
        .and_then(|value| value.get(key))
        .and_then(JsonValue::as_object)
    else {
        return Ok(None);
    };
    let mut map = BTreeMap::new();
    for (env_key, env_value) in object {
        let Some(env_value) = env_value.as_str() else {
            return Err(vm_err(format!(
                "ACP provider env override `{env_key}` must be a string"
            )));
        };
        map.insert(env_key.clone(), env_value.to_string());
    }
    Ok(Some(map))
}

#[cfg(test)]
mod tests {
    use super::*;

    use tokio::io::{split, AsyncBufReadExt, AsyncWriteExt, BufReader};

    async fn read_json<R>(lines: &mut tokio::io::Lines<BufReader<R>>) -> JsonValue
    where
        R: tokio::io::AsyncRead + Unpin,
    {
        let line = lines.next_line().await.expect("read line").expect("line");
        serde_json::from_str(&line).expect("json")
    }

    async fn write_json<W>(writer: &mut W, value: JsonValue)
    where
        W: tokio::io::AsyncWrite + Unpin,
    {
        writer
            .write_all(serde_json::to_string(&value).unwrap().as_bytes())
            .await
            .expect("write json");
        writer.write_all(b"\n").await.expect("write newline");
        writer.flush().await.expect("flush");
    }

    #[test]
    fn acp_prompt_projection_drops_private_reasoning_blocks() {
        let mut request = crate::llm::api::options::base_opts("codex-acp");
        request.model = "default".to_string();
        request.messages = vec![json!({
            "role": "user",
            "content": [
                {"type": "reasoning", "text": "hidden plan", "visibility": "private"},
                {"type": "text", "text": "visible question", "visibility": "public"},
                {
                    "type": "tool_result",
                    "tool_use_id": "call_1",
                    "visibility": "internal",
                    "content": [
                        {"type": "thinking", "text": "nested hidden"},
                        {"type": "text", "text": "visible tool result", "visibility": "public"}
                    ]
                }
            ],
        })];
        let payload = LlmRequestPayload::from(&request);

        let prompt = request_prompt_text(&payload);

        assert!(prompt.contains("visible question"));
        assert!(prompt.contains("visible tool result"));
        assert!(!prompt.contains("hidden plan"));
        assert!(!prompt.contains("nested hidden"));
    }

    #[tokio::test]
    async fn acp_session_collects_chunks_and_cancels_permission_requests() {
        let (client, server) = tokio::io::duplex(16 * 1024);
        let (client_read, client_write) = split(client);
        let (server_read, mut server_write) = split(server);
        let server_task = tokio::spawn(async move {
            let mut lines = BufReader::new(server_read).lines();

            let initialize = read_json(&mut lines).await;
            assert_eq!(initialize["method"], "initialize");
            write_json(
                &mut server_write,
                json!({
                    "jsonrpc": "2.0",
                    "id": initialize["id"].clone(),
                    "result": {
                        "protocolVersion": 1,
                        "agentCapabilities": {},
                        "agentInfo": {"name": "fake-acp"},
                    }
                }),
            )
            .await;

            let session_new = read_json(&mut lines).await;
            assert_eq!(session_new["method"], "session/new");
            let cwd = session_new["params"]["cwd"].as_str().unwrap();
            assert!(std::path::Path::new(cwd).is_absolute());
            write_json(
                &mut server_write,
                json!({
                    "jsonrpc": "2.0",
                    "id": session_new["id"].clone(),
                    "result": {"sessionId": "sess-test"}
                }),
            )
            .await;

            let prompt = read_json(&mut lines).await;
            assert_eq!(prompt["method"], "session/prompt");
            let prompt_text = prompt["params"]["prompt"][0]["text"].as_str().unwrap();
            assert!(prompt_text.contains("System:"));
            assert!(prompt_text.contains("hello"));
            write_json(
                &mut server_write,
                json!({
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": "sess-test",
                        "update": {
                            "sessionUpdate": "agent_message_chunk",
                            "content": {"type": "text", "text": "alpha"}
                        }
                    }
                }),
            )
            .await;
            write_json(
                &mut server_write,
                json!({
                    "jsonrpc": "2.0",
                    "id": 99,
                    "method": crate::llm::acp_permission::METHOD_REQUEST_PERMISSION,
                    "params": {
                        "sessionId": "sess-test",
                        "toolCall": {"toolCallId": "tool-1", "title": "write"},
                        "options": [
                            {
                                "optionId": crate::llm::acp_permission::OPTION_ALLOW,
                                "name": "Allow",
                                "kind": "allow_once"
                            },
                            {
                                "optionId": crate::llm::acp_permission::OPTION_REJECT,
                                "name": "Reject",
                                "kind": "reject_once"
                            }
                        ]
                    }
                }),
            )
            .await;
            let permission = read_json(&mut lines).await;
            assert_eq!(permission["id"], 99);
            assert_eq!(permission["result"]["outcome"]["outcome"], "cancelled");
            write_json(
                &mut server_write,
                json!({
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": "sess-test",
                        "update": {
                            "sessionUpdate": "agent_message_chunk",
                            "content": {"type": "text", "text": " beta"}
                        }
                    }
                }),
            )
            .await;
            write_json(
                &mut server_write,
                json!({
                    "jsonrpc": "2.0",
                    "id": prompt["id"].clone(),
                    "result": {"stopReason": "end_turn"}
                }),
            )
            .await;
        });

        let mut request = crate::llm::api::options::base_opts("codex-acp");
        request.model = "default".to_string();
        request.system = Some("Be concise.".to_string());
        request.messages = vec![json!({"role": "user", "content": "hello"})];
        let payload = LlmRequestPayload::from(&request);
        let runtime = AcpProviderRuntime {
            provider: "codex-acp".to_string(),
            command: "unused".to_string(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: absolute_cwd(Some(".".to_string())).unwrap(),
            mcp_servers: Vec::new(),
            client_capabilities: json!({}),
            client_info: json!({"name": "harn-test"}),
        };
        let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel();
        let result = run_acp_session(
            BufReader::new(client_read),
            client_write,
            runtime,
            &payload,
            Some(delta_tx),
        )
        .await
        .expect("ACP session");

        server_task.await.expect("server task");
        assert_eq!(result.text, "alpha beta");
        assert_eq!(result.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(delta_rx.try_recv().unwrap(), "alpha");
        assert_eq!(delta_rx.try_recv().unwrap(), " beta");
    }
}
