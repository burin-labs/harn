//! Ollama provider — local Ollama server with NDJSON streaming.

use crate::llm::api::{DeltaSender, LlmRequestPayload, LlmResult};
use crate::llm::provider::{LlmProvider, LlmProviderChat};
use crate::value::{VmError, VmValue};
use std::rc::Rc;

/// Zero-cost unit struct for the Ollama provider.
pub(crate) struct OllamaProvider;

impl LlmProvider for OllamaProvider {
    fn name(&self) -> &str {
        "ollama"
    }

    fn is_local(&self) -> bool {
        true
    }
}

impl LlmProviderChat for OllamaProvider {
    fn chat<'a>(
        &'a self,
        request: &'a LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<LlmResult, VmError>> + 'a>> {
        Box::pin(self.chat_impl(request, delta_tx))
    }
}

impl OllamaProvider {
    pub(crate) fn classify_http_error(
        status: reqwest::StatusCode,
        retry_after: Option<&str>,
        body: &str,
    ) -> crate::llm::api::LlmErrorInfo {
        crate::llm::api::classify_provider_http_error("ollama", status, retry_after, body)
    }

    /// Build the Ollama-specific request body. Ollama uses OpenAI-style messages
    /// but with additional options and NDJSON streaming.
    pub(crate) fn build_request_body(opts: &LlmRequestPayload) -> serde_json::Value {
        let mut body =
            crate::llm::providers::OpenAiCompatibleProvider::build_request_body(opts, true);

        if opts.response_format.as_deref() == Some("json") {
            body.as_object_mut()
                .map(|obj| obj.remove("response_format"));
            if let Some(schema) = opts.json_schema.clone() {
                body["format"] = schema;
            } else {
                body["format"] = serde_json::json!("json");
            }
        }

        if body["options"].get("min_p").is_none() {
            body["options"]["min_p"] = serde_json::json!(0.05);
        }
        if body["options"].get("repeat_penalty").is_none() {
            body["options"]["repeat_penalty"] = serde_json::json!(1.05);
        }
        if body["options"].get("num_predict").is_none() && opts.max_tokens > 0 {
            body["options"]["num_predict"] = serde_json::json!(opts.max_tokens);
        }
        // Ollama templates (qwen3:30b-a3b etc.) gate `<think>` emission
        // on the top-level `think` field, NOT
        // `chat_template_kwargs.enable_thinking`. The OpenAI-compat shim
        // passes `think` through to the same template context. Default
        // false for fast tool-call-shaped turns; callers who want
        // reasoning set `thinking` explicitly.
        body["think"] = serde_json::json!(opts.thinking.is_enabled());
        crate::llm::api::apply_ollama_runtime_settings(&mut body, opts.provider_overrides.as_ref());
        body
    }

    pub(crate) fn should_route_via_raw_generate(opts: &LlmRequestPayload) -> bool {
        let caps = crate::llm::capabilities::lookup(&opts.provider, &opts.model);
        caps.recommended_endpoint.as_deref() == Some("/api/generate-raw")
            && !caps.text_tool_wire_format_supported
            && opts
                .native_tools
                .as_ref()
                .is_none_or(|tools| tools.is_empty())
    }

    pub(crate) fn build_raw_generate_body(opts: &LlmRequestPayload) -> serde_json::Value {
        let mut options = serde_json::Map::new();
        if let Some(temp) = opts.temperature {
            options.insert("temperature".to_string(), serde_json::json!(temp));
        }
        if let Some(top_p) = opts.top_p {
            options.insert("top_p".to_string(), serde_json::json!(top_p));
        }
        if let Some(top_k) = opts.top_k {
            options.insert("top_k".to_string(), serde_json::json!(top_k));
        }
        if let Some(seed) = opts.seed {
            options.insert("seed".to_string(), serde_json::json!(seed));
        }
        if let Some(stop) = &opts.stop {
            options.insert("stop".to_string(), serde_json::json!(stop));
        }
        if opts.max_tokens > 0 {
            options.insert(
                "num_predict".to_string(),
                serde_json::json!(opts.max_tokens),
            );
        }

        let mut body = serde_json::json!({
            "model": opts.model,
            "prompt": render_qwen_chat_prompt(opts),
            "stream": opts.stream,
            "raw": true,
            "options": options,
        });
        if opts.response_format.as_deref() == Some("json") {
            if let Some(schema) = opts.json_schema.clone() {
                body["format"] = schema;
            } else {
                body["format"] = serde_json::json!("json");
            }
        }
        crate::llm::api::apply_ollama_runtime_settings(&mut body, opts.provider_overrides.as_ref());
        body
    }

    /// The actual chat implementation.
    pub(crate) async fn chat_impl(
        &self,
        request: &LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> Result<LlmResult, VmError> {
        if Self::should_route_via_raw_generate(request) {
            return self.raw_generate_chat_impl(request, delta_tx).await;
        }
        let body = Self::build_request_body(request);
        crate::llm::api::vm_call_llm_api_with_body(
            request, delta_tx, body, false, // is_anthropic_style
            true,  // is_ollama
        )
        .await
    }

    async fn raw_generate_chat_impl(
        &self,
        request: &LlmRequestPayload,
        delta_tx: Option<DeltaSender>,
    ) -> Result<LlmResult, VmError> {
        let body = Self::build_raw_generate_body(request);
        let pdef = crate::llm_config::provider_config(&request.provider);
        let base_url = pdef
            .as_ref()
            .map(crate::llm_config::resolve_base_url)
            .unwrap_or_else(|| "http://localhost:11434".to_string());
        let endpoint = pdef
            .as_ref()
            .and_then(|provider| provider.completion_endpoint.as_deref())
            .unwrap_or("/api/generate");
        let client = if request.stream {
            crate::llm::shared_streaming_client().clone()
        } else {
            crate::llm::shared_blocking_client().clone()
        };
        let req = client
            .post(format!("{base_url}{endpoint}"))
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(request.resolve_timeout()))
            .json(&body);
        let req = crate::llm::api::apply_auth_headers(req, &request.api_key, pdef.as_ref());
        let response = req.send().await.map_err(|error| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "ollama raw generate API error: {error}"
            ))))
        })?;
        if !response.status().is_success() {
            let status = response.status();
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let body = response.text().await.unwrap_or_default();
            let msg = Self::classify_http_error(status, retry_after.as_deref(), &body).message;
            return Err(VmError::Thrown(VmValue::String(Rc::from(msg))));
        }
        if request.stream {
            let tx = delta_tx.unwrap_or_else(|| {
                let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<String>();
                tx
            });
            parse_raw_generate_stream(response, &request.provider, &request.model, tx).await
        } else {
            parse_raw_generate_response(response, request).await
        }
    }
}

fn render_qwen_chat_prompt(opts: &LlmRequestPayload) -> String {
    let mut rendered = String::new();
    if let Some(system) = opts.system.as_deref().filter(|value| !value.is_empty()) {
        push_qwen_message(&mut rendered, "system", system, true);
    }
    for message in &opts.messages {
        let role = message
            .get("role")
            .and_then(|value| value.as_str())
            .unwrap_or("user");
        let role = match role {
            "assistant" | "system" | "user" => role,
            // Raw text-mode conversations should not rely on provider
            // native tool roles. Make tool outputs visible as ordinary
            // user context if a restored transcript contains them.
            _ => "user",
        };
        let content = render_message_text(message);
        push_qwen_message(&mut rendered, role, &content, true);
    }
    if let Some(prefill) = opts.prefill.as_deref() {
        push_qwen_message(&mut rendered, "assistant", prefill, false);
    } else {
        rendered.push_str("<|im_start|>assistant\n");
    }
    rendered
}

fn push_qwen_message(out: &mut String, role: &str, content: &str, close: bool) {
    out.push_str("<|im_start|>");
    out.push_str(role);
    out.push('\n');
    out.push_str(content);
    if close {
        out.push_str("\n<|im_end|>\n");
    }
}

fn render_message_text(message: &serde_json::Value) -> String {
    let mut text = render_content_text(message.get("content").unwrap_or(&serde_json::Value::Null));
    if let Some(tool_name) = message.get("tool_name").and_then(|value| value.as_str()) {
        text = format!("[tool result: {tool_name}]\n{text}");
    } else if let Some(tool_call_id) = message.get("tool_call_id").and_then(|value| value.as_str())
    {
        text = format!("[tool result: {tool_call_id}]\n{text}");
    }
    if let Some(tool_calls) = message.get("tool_calls").and_then(|value| value.as_array()) {
        if text.trim().is_empty() {
            text.push_str("[assistant tool calls]\n");
        } else {
            text.push_str("\n\n[assistant tool calls]\n");
        }
        text.push_str(&serde_json::to_string(tool_calls).unwrap_or_default());
    }
    text
}

fn render_content_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(blocks) => {
            let mut rendered = String::new();
            for block in blocks {
                let block_type = block.get("type").and_then(|value| value.as_str());
                if matches!(block_type, Some("reasoning" | "thinking")) {
                    continue;
                }
                let fragment = match block_type {
                    Some("text" | "output_text") => block
                        .get("text")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    Some("tool_result") => {
                        let content = block
                            .get("content")
                            .and_then(|value| value.as_str())
                            .unwrap_or_default();
                        format!("[tool result]\n{content}")
                    }
                    Some("tool_call") => serde_json::to_string(block).unwrap_or_default(),
                    _ => block
                        .get("text")
                        .and_then(|value| value.as_str())
                        .map(str::to_string)
                        .unwrap_or_else(|| block.to_string()),
                };
                if fragment.is_empty() {
                    continue;
                }
                if !rendered.is_empty() {
                    rendered.push_str("\n\n");
                }
                rendered.push_str(&fragment);
            }
            rendered
        }
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

async fn parse_raw_generate_response(
    response: reqwest::Response,
    request: &LlmRequestPayload,
) -> Result<LlmResult, VmError> {
    let json: serde_json::Value = response.json().await.map_err(|error| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "ollama raw generate response parse error: {error}"
        ))))
    })?;
    if let Some(error) = json.get("error").and_then(|value| value.as_str()) {
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "ollama raw generate API error: {error}"
        )))));
    }
    let raw = json
        .get("response")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let (mut text, thinking) = crate::llm::api::split_openai_thinking_blocks(raw);
    if text.is_empty() && !thinking.is_empty() {
        text = thinking.clone();
    }
    let input_tokens = json
        .get("prompt_eval_count")
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    let output_tokens = json
        .get("eval_count")
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    if text.is_empty() && output_tokens > 0 {
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "ollama raw-generate model {} reported eval_count={output_tokens} but delivered no content or thinking",
            request.model
        )))));
    }
    Ok(LlmResult {
        blocks: blocks_from_text_and_thinking(&text, &thinking),
        text,
        tool_calls: Vec::new(),
        input_tokens,
        output_tokens,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        model: json
            .get("model")
            .and_then(|value| value.as_str())
            .unwrap_or(&request.model)
            .to_string(),
        provider: request.provider.clone(),
        thinking: (!thinking.is_empty()).then_some(thinking),
        stop_reason: json
            .get("done_reason")
            .and_then(|value| value.as_str())
            .map(str::to_string),
    })
}

async fn parse_raw_generate_stream(
    response: reqwest::Response,
    provider: &str,
    model: &str,
    delta_tx: DeltaSender,
) -> Result<LlmResult, VmError> {
    use tokio::io::AsyncBufReadExt;
    use tokio_stream::StreamExt;

    let stream = response.bytes_stream();
    let reader = tokio::io::BufReader::new(tokio_util::io::StreamReader::new(
        stream.map(|result| result.map_err(std::io::Error::other)),
    ));
    let mut lines = reader.lines();
    let mut splitter = crate::llm::api::ThinkingStreamSplitter::new();
    let mut text = String::new();
    let mut result_model = model.to_string();
    let mut input_tokens = 0;
    let mut output_tokens = 0;
    let mut stop_reason = None;

    while let Ok(Some(line)) = lines.next_line().await {
        if line.is_empty() {
            continue;
        }
        let json: serde_json::Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if let Some(error) = json.get("error").and_then(|value| value.as_str()) {
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "ollama raw generate API error: {error}"
            )))));
        }
        if let Some(chunk) = json.get("response").and_then(|value| value.as_str()) {
            let visible = splitter.push(chunk);
            if !visible.is_empty() {
                text.push_str(&visible);
                let _ = delta_tx.send(visible);
            }
        }
        if let Some(value) = json.get("model").and_then(|value| value.as_str()) {
            result_model = value.to_string();
        }
        if json.get("done").and_then(|value| value.as_bool()) == Some(true) {
            input_tokens = json
                .get("prompt_eval_count")
                .and_then(|value| value.as_i64())
                .unwrap_or(0);
            output_tokens = json
                .get("eval_count")
                .and_then(|value| value.as_i64())
                .unwrap_or(0);
            stop_reason = json
                .get("done_reason")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            break;
        }
    }
    let tail = splitter.flush();
    if !tail.is_empty() {
        text.push_str(&tail);
        let _ = delta_tx.send(tail);
    }
    let thinking = splitter.thinking.trim().to_string();
    if text.is_empty() && !thinking.is_empty() {
        text = thinking.clone();
    }
    if text.is_empty() && output_tokens > 0 {
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "ollama raw-generate model {model} reported eval_count={output_tokens} but delivered no content or thinking"
        )))));
    }
    Ok(LlmResult {
        blocks: blocks_from_text_and_thinking(&text, &thinking),
        text,
        tool_calls: Vec::new(),
        input_tokens,
        output_tokens,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        model: result_model,
        provider: provider.to_string(),
        thinking: (!thinking.is_empty()).then_some(thinking),
        stop_reason,
    })
}

fn blocks_from_text_and_thinking(text: &str, thinking: &str) -> Vec<serde_json::Value> {
    let mut blocks = Vec::new();
    if !thinking.is_empty() {
        blocks.push(serde_json::json!({
            "type": "reasoning",
            "text": thinking,
            "visibility": "private",
        }));
    }
    if !text.is_empty() {
        blocks.push(serde_json::json!({
            "type": "output_text",
            "text": text,
            "visibility": "public",
        }));
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::api::{LlmErrorKind, LlmErrorReason, ThinkingConfig};

    struct ScopedEnvVar {
        key: &'static str,
        previous: Option<String>,
    }

    impl ScopedEnvVar {
        fn remove(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            unsafe {
                std::env::remove_var(key);
            }
            Self { key, previous }
        }
    }

    impl Drop for ScopedEnvVar {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    fn base_payload() -> LlmRequestPayload {
        LlmRequestPayload {
            provider: "ollama".to_string(),
            model: "qwen3.5:35b-a3b-coding-nvfp4".to_string(),
            api_key: String::new(),
            fallback_chain: Vec::new(),
            session_id: None,
            messages: vec![serde_json::json!({"role": "user", "content": "hello"})],
            system: None,
            max_tokens: 64,
            temperature: Some(0.0),
            top_p: None,
            top_k: None,
            stop: None,
            seed: None,
            frequency_penalty: None,
            presence_penalty: None,
            response_format: Some("json".to_string()),
            json_schema: Some(serde_json::json!({"type": "object"})),
            thinking: ThinkingConfig::Disabled,
            native_tools: None,
            tool_choice: None,
            cache: false,
            timeout: None,
            stream: true,
            provider_overrides: None,
            prefill: None,
        }
    }

    #[test]
    fn json_response_format_maps_to_ollama_format_field() {
        let body = OllamaProvider::build_request_body(&base_payload());
        assert_eq!(body["format"], serde_json::json!({"type": "object"}));
        assert!(body.get("response_format").is_none());
    }

    #[test]
    fn plain_requests_do_not_emit_format_field() {
        let mut payload = base_payload();
        payload.response_format = None;
        payload.json_schema = None;
        let body = OllamaProvider::build_request_body(&payload);
        assert!(body.get("format").is_none());
    }

    #[test]
    fn defaults_ollama_runtime_settings() {
        let _guard = crate::llm::env_lock().lock().expect("env lock");
        let _env = [
            ScopedEnvVar::remove("HARN_OLLAMA_NUM_CTX"),
            ScopedEnvVar::remove("OLLAMA_CONTEXT_LENGTH"),
            ScopedEnvVar::remove("OLLAMA_NUM_CTX"),
            ScopedEnvVar::remove("HARN_OLLAMA_KEEP_ALIVE"),
            ScopedEnvVar::remove("OLLAMA_KEEP_ALIVE"),
        ];
        let mut payload = base_payload();
        payload.response_format = None;
        payload.json_schema = None;
        let body = OllamaProvider::build_request_body(&payload);
        assert_eq!(body["options"]["num_ctx"], serde_json::json!(32768));
        assert_eq!(body["keep_alive"], serde_json::json!("30m"));
    }

    #[test]
    fn maps_provider_runtime_overrides_to_ollama_body() {
        let mut payload = base_payload();
        payload.provider_overrides = Some(serde_json::json!({
            "num_ctx": 65536,
            "keep_alive": "forever",
            "options": {"top_k": 40},
            "think": true,
        }));
        let body = OllamaProvider::build_request_body(&payload);
        assert_eq!(body["options"]["num_ctx"], serde_json::json!(65536));
        assert_eq!(body["options"]["top_k"], serde_json::json!(40));
        assert_eq!(body["keep_alive"], serde_json::json!(-1));
        assert_eq!(body["think"], serde_json::json!(true));
        assert!(body.get("num_ctx").is_none());
    }

    #[test]
    fn qwen_text_tool_route_uses_raw_generate_bypass() {
        let mut payload = base_payload();
        payload.response_format = None;
        payload.json_schema = None;
        payload.native_tools = None;

        assert!(OllamaProvider::should_route_via_raw_generate(&payload));
        let body = OllamaProvider::build_raw_generate_body(&payload);
        assert_eq!(body["raw"], serde_json::json!(true));
        assert_eq!(
            body["model"],
            serde_json::json!("qwen3.5:35b-a3b-coding-nvfp4")
        );
        let prompt = body["prompt"].as_str().unwrap_or_default();
        assert!(prompt.contains("<|im_start|>user\nhello\n<|im_end|>"));
        assert!(prompt.ends_with("<|im_start|>assistant\n"));
        assert!(body.get("chat_template_kwargs").is_none());
    }

    #[test]
    fn qwen_native_tool_route_stays_on_ollama_chat() {
        let mut payload = base_payload();
        payload.response_format = None;
        payload.json_schema = None;
        payload.native_tools = Some(vec![serde_json::json!({
            "type": "function",
            "function": {"name": "read"}
        })]);

        assert!(!OllamaProvider::should_route_via_raw_generate(&payload));
    }

    #[test]
    fn classifies_ollama_missing_model_as_terminal_model_unavailable() {
        let info = OllamaProvider::classify_http_error(
            reqwest::StatusCode::NOT_FOUND,
            None,
            r#"{"error":"model not found"}"#,
        );
        assert_eq!(info.kind, LlmErrorKind::Terminal);
        assert_eq!(info.reason, LlmErrorReason::ModelUnavailable);
    }

    #[test]
    fn classifies_ollama_timeout_as_transient_timeout() {
        let info = OllamaProvider::classify_http_error(
            reqwest::StatusCode::GATEWAY_TIMEOUT,
            None,
            r#"{"error":"upstream timeout"}"#,
        );
        assert_eq!(info.kind, LlmErrorKind::Transient);
        assert_eq!(info.reason, LlmErrorReason::Timeout);
    }

    #[test]
    fn raw_generate_prompt_continues_prefill_without_end_token() {
        let mut payload = base_payload();
        payload.response_format = None;
        payload.json_schema = None;
        payload.prefill = Some("<tool_call>\nedit(".to_string());
        let body = OllamaProvider::build_raw_generate_body(&payload);
        let prompt = body["prompt"].as_str().unwrap_or_default();
        assert!(prompt.ends_with("<|im_start|>assistant\n<tool_call>\nedit("));
        assert!(!prompt.ends_with("<|im_end|>\n"));
    }
}
