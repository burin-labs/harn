//! ACP capability metadata and prompt content normalization.
pub const ACP_SCHEMA_COMPATIBILITY: &str =
    "agentclientprotocol/agent-client-protocol schema v0.12.2";

pub const ACP_SESSION_UPDATE_VARIANTS: &[&str] = &[
    "user_message_chunk",
    "agent_message_chunk",
    "agent_thought_chunk",
    "tool_call",
    "tool_call_update",
    "plan",
    "available_commands_update",
    "current_mode_update",
    "config_option_update",
    "session_info_update",
    "session_truncated",
];

pub const HARN_SESSION_UPDATE_EXTENSIONS: &[&str] = &[
    "available_commands_update",
    "fs_watch",
    "handoff",
    "hitl_request",
    "hitl_resolved",
    "log",
    "progress",
    "skill_activated",
    "skill_deactivated",
    "skill_scope_tools",
    "tool_search_query",
    "tool_search_result",
    "transcript_compacted",
    "worker_update",
];

/// JSON-RPC method name for the ACP `ExtNotification` envelope that
/// carries Harn pipeline-loop milestones. The leading `_` puts it in
/// the ACP-reserved extension namespace, so strict clients that don't
/// know the method MUST ignore it gracefully (per the ACP
/// extensibility spec). Callers should never hardcode the literal —
/// reference this constant so a future rename ripples through the
/// adapter, fixtures, tests, and capability advertisement together.
pub const HARN_AGENT_EVENT_METHOD: &str = "_harn/agentEvent";

/// Pipeline-loop milestone kinds the adapter currently emits via
/// `_harn/agentEvent`. The list is stable wire vocabulary — adding a
/// new kind is additive and SHOULD be treated by clients as
/// "unknown kind, ignore." Keep it sorted for diff-friendliness and
/// keep it in lockstep with the match arm in `events.rs`.
pub const HARN_AGENT_EVENT_KINDS: &[&str] = &[
    "budget_exhausted",
    "composition_child_call",
    "composition_child_result",
    "composition_error",
    "composition_finish",
    "composition_start",
    "daemon_watchdog_tripped",
    "feedback_injected",
    "judge_decision",
    "loop_control_decision",
    "loop_stuck",
    "progress_reported",
    "session_closed",
    "tool_call_audit",
    "typed_checkpoint",
    "turn_end",
    "turn_start",
];

pub const HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS: &[&str] = &[
    "audit",
    "durationMs",
    "error",
    "errorCategory",
    "executionDurationMs",
    "executor",
    "parsing",
    "rawInputPartial",
];

pub const HARN_CONTENT_EXTENSION_FIELDS: &[&str] = &["visible_delta", "visible_text"];
pub(super) fn harn_acp_extension_meta() -> serde_json::Value {
    serde_json::json!({
        "harn": {
            "schemaCompatibility": ACP_SCHEMA_COMPATIBILITY,
            "sessionUpdateExtensions": HARN_SESSION_UPDATE_EXTENSIONS,
            "toolLifecycleExtensionFields": HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS,
            "contentExtensionFields": HARN_CONTENT_EXTENSION_FIELDS,
            // ACP `ExtNotification` methods this server emits beyond the
            // canonical `session/update` stream. Clients that recognize
            // the method consume the payload; clients that don't MUST
            // ignore it (per ACP extensibility spec). Keys are method
            // names; values are static descriptors so a client can
            // version-check before subscribing.
            "extensionMethods": {
                HARN_AGENT_EVENT_METHOD: {
                    "description": "Pipeline-loop milestones that have no \
                                    canonical ACP session/update mapping.",
                    "kinds": HARN_AGENT_EVENT_KINDS,
                    "schema": "https://harnlang.com/spec/harn-extensions/agent-event/v1",
                },
            },
            "hostCapabilityOperations": {
                "process": [
                    "exec",
                    "list_shells",
                    "get_default_shell",
                    "set_default_shell",
                    "shell_invocation"
                ]
            },
            "extensionContract": "https://harnlang.com/spec/harn-extensions/v1",
        }
    })
}

pub(super) fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn configured_llm_route_for_capabilities() -> (String, String) {
    let provider = non_empty_env("HARN_LLM_PROVIDER")
        .filter(|provider| !provider.eq_ignore_ascii_case("auto"))
        .or_else(|| {
            if std::env::var("LOCAL_LLM_BASE_URL").is_ok()
                && (non_empty_env("HARN_LLM_MODEL").is_some()
                    || non_empty_env("LOCAL_LLM_MODEL").is_some())
            {
                Some("local".to_string())
            } else {
                None
            }
        })
        .or_else(|| {
            non_empty_env("HARN_LLM_MODEL").map(|model| {
                let resolved = harn_vm::llm_config::resolve_model_info(&model);
                resolved.provider
            })
        })
        .unwrap_or_else(harn_vm::llm_config::default_provider);

    let raw_model = non_empty_env("HARN_LLM_MODEL").or_else(|| {
        if provider == "local" {
            non_empty_env("LOCAL_LLM_MODEL")
        } else {
            None
        }
    });
    let model = raw_model
        .map(|model| harn_vm::llm_config::resolve_model(&model).0)
        .unwrap_or_else(|| harn_vm::llm_config::default_model_for_provider(&provider));

    (provider, model)
}

pub(super) fn acp_prompt_capabilities() -> serde_json::Value {
    let (provider, model) = configured_llm_route_for_capabilities();
    let capabilities = harn_vm::llm::capabilities::lookup(&provider, &model);
    serde_json::json!({
        "image": capabilities.vision || capabilities.vision_supported,
        "audio": capabilities.audio,
        "embeddedContext": capabilities.pdf || capabilities.files_api_supported,
    })
}

pub(super) fn acp_agent_capabilities() -> serde_json::Value {
    serde_json::json!({
        "_meta": harn_acp_extension_meta(),
        "loadSession": true,
        "promptCapabilities": acp_prompt_capabilities(),
        "mcpCapabilities": {
            "http": true,
            "sse": true,
        },
        "sessionCapabilities": {
            "list": {},
        },
    })
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct NormalizedAcpPrompt {
    pub(super) text: String,
    pub(super) content: Vec<serde_json::Value>,
    pub(super) messages: Vec<serde_json::Value>,
}

pub(super) fn normalize_acp_prompt(
    params: &serde_json::Value,
) -> Result<NormalizedAcpPrompt, String> {
    let Some(prompt) = params.get("prompt") else {
        return Ok(NormalizedAcpPrompt {
            text: String::new(),
            content: Vec::new(),
            messages: prompt_messages_for_content(&[]),
        });
    };
    let blocks = prompt.as_array().ok_or_else(|| {
        "session/prompt: prompt must be an array of ACP content blocks".to_string()
    })?;

    let mut content = Vec::new();
    for block in blocks {
        content.push(normalize_acp_prompt_block(block)?);
    }

    let text = prompt_text_from_content(&content);
    let messages = prompt_messages_for_content(&content);
    Ok(NormalizedAcpPrompt {
        text,
        content,
        messages,
    })
}

pub(super) fn normalize_acp_prompt_block(
    block: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    match block.get("type").and_then(|value| value.as_str()) {
        Some("text") => Ok(serde_json::json!({
            "type": "text",
            "text": required_string(block, "text", "text prompt block")?,
        })),
        Some("image") => normalize_binary_prompt_block(block, "image"),
        Some("audio") => normalize_binary_prompt_block(block, "audio"),
        Some("resource") => normalize_embedded_resource_block(block),
        Some("resource_link") => normalize_resource_link_block(block),
        Some(other) => Err(format!(
            "session/prompt: unsupported content block type `{other}`"
        )),
        None => Err("session/prompt: content block is missing required `type`".to_string()),
    }
}

pub(super) fn normalize_binary_prompt_block(
    block: &serde_json::Value,
    block_type: &str,
) -> Result<serde_json::Value, String> {
    let media_type = required_media_type(block, block_type)?;
    let data = block
        .get("data")
        .or_else(|| block.get("base64"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty());
    let uri = block
        .get("uri")
        .or_else(|| block.get("url"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty());

    let mut normalized = serde_json::json!({
        "type": block_type,
        "media_type": media_type,
    });
    if let Some(data) = data {
        normalized["base64"] = serde_json::json!(data);
        if let Some(uri) = uri {
            normalized["source_uri"] = serde_json::json!(uri);
        }
    } else if let Some(uri) = uri {
        normalized["url"] = serde_json::json!(uri);
    } else {
        return Err(format!(
            "session/prompt: {block_type} block requires `data` or `uri`"
        ));
    }
    if block_type == "image" {
        if let Some(detail) = block.get("detail").and_then(|value| value.as_str()) {
            normalized["detail"] = serde_json::json!(detail);
        }
    }
    Ok(normalized)
}

pub(super) fn normalize_embedded_resource_block(
    block: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let resource = block
        .get("resource")
        .and_then(|value| value.as_object())
        .ok_or_else(|| "session/prompt: resource block requires `resource` object".to_string())?;
    let uri = resource
        .get("uri")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "session/prompt: embedded resource requires `uri`".to_string())?;
    let media_type = resource
        .get("mimeType")
        .or_else(|| resource.get("media_type"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty());

    if let Some(text) = resource.get("text").and_then(|value| value.as_str()) {
        return Ok(serde_json::json!({
            "type": "text",
            "text": render_embedded_text_resource(uri, media_type, text),
            "uri": uri,
            "media_type": media_type,
        }));
    }

    let blob = resource
        .get("blob")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "session/prompt: embedded resource requires `text` or `blob`".to_string())?;
    let Some(media_type) = media_type else {
        return Ok(serde_json::json!({
            "type": "text",
            "text": format!("Embedded binary resource: {uri}\nMIME type: unknown"),
            "uri": uri,
        }));
    };
    if media_type.starts_with("image/") {
        Ok(serde_json::json!({
            "type": "image",
            "base64": blob,
            "media_type": media_type,
            "source_uri": uri,
        }))
    } else if media_type.starts_with("audio/") {
        Ok(serde_json::json!({
            "type": "audio",
            "base64": blob,
            "media_type": media_type,
            "source_uri": uri,
        }))
    } else if media_type == "application/pdf" {
        Ok(serde_json::json!({
            "type": "pdf",
            "base64": blob,
            "media_type": media_type,
            "source_uri": uri,
        }))
    } else {
        Ok(serde_json::json!({
            "type": "text",
            "text": format!("Embedded binary resource: {uri}\nMIME type: {media_type}"),
            "uri": uri,
            "media_type": media_type,
        }))
    }
}

pub(super) fn normalize_resource_link_block(
    block: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let uri = required_string(block, "uri", "resource_link prompt block")?;
    let mut lines = vec![format!("Resource link: {uri}")];
    for key in ["name", "title", "description", "mimeType", "media_type"] {
        if let Some(value) = block.get(key).and_then(|value| value.as_str()) {
            if !value.is_empty() {
                lines.push(format!("{key}: {value}"));
            }
        }
    }
    if let Some(size) = block.get("size").and_then(|value| value.as_u64()) {
        lines.push(format!("size: {size}"));
    }
    Ok(serde_json::json!({
        "type": "text",
        "text": lines.join("\n"),
        "uri": uri,
    }))
}

pub(super) fn render_embedded_text_resource(
    uri: &str,
    media_type: Option<&str>,
    text: &str,
) -> String {
    let mut rendered = format!("Embedded resource: {uri}");
    if let Some(media_type) = media_type {
        rendered.push_str(&format!("\nMIME type: {media_type}"));
    }
    rendered.push_str("\n\n");
    rendered.push_str(text);
    rendered
}

pub(super) fn required_media_type(
    block: &serde_json::Value,
    block_type: &str,
) -> Result<String, String> {
    block
        .get("mimeType")
        .or_else(|| block.get("media_type"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("session/prompt: {block_type} block requires `mimeType`"))
}

pub(super) fn required_string(
    value: &serde_json::Value,
    key: &str,
    context: &str,
) -> Result<String, String> {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("session/prompt: {context} requires `{key}`"))
}

pub(super) fn retarget_prompt_text(prompt: &mut NormalizedAcpPrompt, text: String) {
    if let Some(block) = prompt
        .content
        .iter_mut()
        .find(|block| block.get("type").and_then(|value| value.as_str()) == Some("text"))
    {
        block["text"] = serde_json::json!(text);
    } else {
        prompt.content.insert(
            0,
            serde_json::json!({
                "type": "text",
                "text": text,
            }),
        );
    }
    prompt.text = prompt_text_from_content(&prompt.content);
    prompt.messages = prompt_messages_for_content(&prompt.content);
}

pub(super) fn prompt_text_from_content(content: &[serde_json::Value]) -> String {
    content
        .iter()
        .filter_map(|block| {
            if block.get("type").and_then(|value| value.as_str()) == Some("text") {
                block.get("text").and_then(|value| value.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn prompt_messages_for_content(content: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let message_content = if content.is_empty() {
        serde_json::Value::String(String::new())
    } else {
        serde_json::Value::Array(content.to_vec())
    };
    vec![serde_json::json!({
        "role": "user",
        "content": message_content,
    })]
}
