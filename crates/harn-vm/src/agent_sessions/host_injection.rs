use super::*;

impl HostInjectionKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::HostToolResult => "host_tool_result",
            Self::HostAttachment => "host_attachment",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostInjectionRequest {
    pub kind: HostInjectionKind,
    #[serde(default)]
    pub delivery: InjectionDelivery,
    pub payload: serde_json::Value,
    pub provenance: HostInjectionProvenance,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HostToolResultPayload {
    #[serde(default)]
    tool_call_id: Option<String>,
    tool_name: String,
    #[serde(default)]
    kind: Option<ToolKind>,
    #[serde(default)]
    raw_input: serde_json::Value,
    #[serde(default = "default_completed_tool_status")]
    status: ToolCallStatus,
    #[serde(default)]
    raw_output: Option<serde_json::Value>,
    #[serde(default)]
    result_pointer: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    duration_ms: Option<u64>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HostAttachmentPayload {
    media_type: String,
    flavor: AttachmentFlavor,
    artifact_pointer: String,
    sha256: String,
    size_bytes: u64,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    description_model: Option<String>,
}

fn default_completed_tool_status() -> ToolCallStatus {
    ToolCallStatus::Completed
}

pub fn inject_host_event(id: &str, injection: VmValue) -> Result<serde_json::Value, String> {
    let injection_json = crate::llm::helpers::vm_value_to_json(&injection);
    let request: HostInjectionRequest = serde_json::from_value(injection_json)
        .map_err(|error| format!("agent_inject_host_event: invalid injection: {error}"))?;
    inject_host_event_request(id, request)
}

/// Inject a validated host event without crossing the dynamic VM-value boundary.
///
/// Protocol adapters and Rust embedders should use this entry point. Harn code
/// continues to use [`inject_host_event`], which parses into the same contract.
pub fn inject_host_event_request(
    id: &str,
    request: HostInjectionRequest,
) -> Result<serde_json::Value, String> {
    if !exists(id) {
        return Err(format!(
            "agent_inject_host_event: unknown session id '{id}'"
        ));
    }
    validate_host_injection_payload(request.kind, request.payload.clone())?;
    let injection_id = uuid::Uuid::now_v7().to_string();
    if request.delivery == InjectionDelivery::Immediate {
        let sequence = crate::orchestration::agent_inbox::reserve_sequence(id);
        deliver_host_injection_request(id, injection_id.clone(), sequence, request, "immediate")?;
        return Ok(serde_json::json!({
            "injection_id": injection_id,
            "sequence": sequence,
            "delivery": InjectionDelivery::Immediate.as_str(),
            "status": "injected",
        }));
    }
    let queued = serde_json::json!({
        "injection_id": injection_id,
        "request": request,
    });
    let sequence = crate::orchestration::agent_inbox::push_host_injection(
        id,
        request.kind.as_str(),
        queued,
        request.delivery,
        "agent_inject_host_event",
    );
    Ok(serde_json::json!({
        "injection_id": injection_id,
        "sequence": sequence,
        "delivery": request.delivery.as_str(),
        "status": "queued",
    }))
}

pub fn drain_queued_host_injections(
    id: &str,
    delivery: InjectionDelivery,
    seam: &str,
) -> Result<Vec<serde_json::Value>, String> {
    if !exists(id) {
        return Err(format!(
            "agent_inject_host_event: unknown session id '{id}'"
        ));
    }
    let entries = crate::orchestration::agent_inbox::drain_where(id, |entry| {
        entry.payload.is_some() && entry.delivery == Some(delivery)
    });
    let mut delivered = Vec::with_capacity(entries.len());
    for entry in entries {
        let payload = entry.payload.ok_or_else(|| {
            "agent_inject_host_event: queued typed inbox entry missing payload".to_string()
        })?;
        let injection_id = payload
            .get("injection_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                "agent_inject_host_event: queued injection missing injection_id".to_string()
            })?
            .to_string();
        let request_value = payload.get("request").cloned().ok_or_else(|| {
            "agent_inject_host_event: queued injection missing request".to_string()
        })?;
        let request: HostInjectionRequest =
            serde_json::from_value(request_value).map_err(|error| {
                format!("agent_inject_host_event: invalid queued injection: {error}")
            })?;
        deliver_host_injection_request(id, injection_id.clone(), entry.sequence, request, seam)?;
        delivered.push(serde_json::json!({
            "injection_id": injection_id,
            "sequence": entry.sequence,
            "delivery": delivery.as_str(),
            "delivered_at_seam": seam,
            "kind": entry.kind,
            "source": entry.source,
            "ts_ms": entry.ts_ms,
        }));
    }
    Ok(delivered)
}

fn validate_host_injection_payload(
    kind: HostInjectionKind,
    payload: serde_json::Value,
) -> Result<(), String> {
    match kind {
        HostInjectionKind::HostToolResult => {
            let _: HostToolResultPayload = serde_json::from_value(payload).map_err(|error| {
                format!("agent_inject_host_event: invalid host_tool_result payload: {error}")
            })?;
        }
        HostInjectionKind::HostAttachment => {
            let attachment: HostAttachmentPayload =
                serde_json::from_value(payload).map_err(|error| {
                    format!("agent_inject_host_event: invalid host_attachment payload: {error}")
                })?;
            if attachment.media_type.trim().is_empty() {
                return Err(
                    "agent_inject_host_event: host_attachment media_type must not be empty".into(),
                );
            }
            if attachment.artifact_pointer.trim().is_empty() {
                return Err(
                    "agent_inject_host_event: host_attachment artifact_pointer must not be empty"
                        .into(),
                );
            }
            if attachment.sha256.len() != 64
                || !attachment
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
            {
                return Err("agent_inject_host_event: host_attachment sha256 must be a 64-character hex digest".into());
            }
            if attachment.description_model.is_some() && attachment.description.is_none() {
                return Err("agent_inject_host_event: host_attachment description_model requires a recorded description".into());
            }
        }
    }
    Ok(())
}

fn deliver_host_injection_request(
    id: &str,
    injection_id: String,
    sequence: u64,
    request: HostInjectionRequest,
    delivered_at_seam: &str,
) -> Result<(), String> {
    let (message, transcript_event, agent_event) = match request.kind {
        HostInjectionKind::HostToolResult => {
            let payload: HostToolResultPayload =
                serde_json::from_value(request.payload).map_err(|error| {
                    format!("agent_inject_host_event: invalid host_tool_result payload: {error}")
                })?;
            build_host_tool_result_injection(
                id,
                injection_id,
                sequence,
                request.delivery,
                request.provenance,
                payload,
                delivered_at_seam,
            )
        }
        HostInjectionKind::HostAttachment => {
            let payload: HostAttachmentPayload =
                serde_json::from_value(request.payload).map_err(|error| {
                    format!("agent_inject_host_event: invalid host_attachment payload: {error}")
                })?;
            build_host_attachment_injection(
                id,
                injection_id,
                sequence,
                request.delivery,
                request.provenance,
                payload,
                delivered_at_seam,
            )
        }
    };
    inject_typed_message(id, message, transcript_event, agent_event)
}

fn build_host_tool_result_injection(
    session_id: &str,
    injection_id: String,
    sequence: u64,
    delivery: InjectionDelivery,
    provenance: HostInjectionProvenance,
    payload: HostToolResultPayload,
    delivered_at_seam: &str,
) -> (VmValue, VmValue, AgentEvent) {
    let tool_call_id = payload
        .tool_call_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("hosttc_{}", injection_id.replace('-', "")));
    let body = host_tool_result_body(&payload);
    let trust = trust_for_tool(payload.kind, &provenance);
    let origin = format!("host_injected:{}", payload.tool_name);
    let ingress = crate::security::sanitize_ingress(&body, &origin, trust);
    let text = host_injection_envelope(
        "host_tool_result",
        &payload.tool_name,
        &injection_id,
        &ingress.delivered,
    );
    let sanitization = sanitization_verdict(
        trust,
        &serde_json::json!({
            "raw_output": payload.raw_output,
            "result_pointer": payload.result_pointer,
            "error": payload.error,
        }),
        &text,
        SanitizationAction::Passed,
        &ingress,
        None,
    );
    record_host_ingress(session_id, &origin, &tool_call_id, trust, &body, &ingress);
    let metadata = serde_json::json!({
        "injection_id": injection_id,
        "sequence": sequence,
        "provenance": provenance,
        "sanitization": sanitization,
        "tool_call_id": tool_call_id,
        "tool_name": payload.tool_name,
        "result_pointer": payload.result_pointer,
    });
    let message = host_injection_user_message(&injection_id, &text, &metadata, None);
    let transcript_event = crate::llm::helpers::transcript_event(
        "host_tool_result",
        "user",
        "public",
        &text,
        Some(metadata),
    );
    let event = AgentEvent::HostToolResult {
        session_id: session_id.to_string(),
        injection_id,
        tool_call_id,
        tool_name: payload.tool_name,
        kind: payload.kind,
        raw_input: payload.raw_input,
        status: payload.status,
        raw_output: payload.raw_output,
        result_pointer: payload.result_pointer,
        error: payload.error,
        duration_ms: payload.duration_ms,
        delivery,
        delivered_at_seam: Some(delivered_at_seam.to_string()),
        sequence,
        provenance,
        sanitization,
    };
    (message, transcript_event, event)
}

fn build_host_attachment_injection(
    session_id: &str,
    injection_id: String,
    sequence: u64,
    delivery: InjectionDelivery,
    provenance: HostInjectionProvenance,
    payload: HostAttachmentPayload,
    delivered_at_seam: &str,
) -> (VmValue, VmValue, AgentEvent) {
    let materialized =
        crate::host_attachments::materialize(&payload.artifact_pointer, &payload.media_type);
    let (rendered, body, image) = select_attachment_rendering(session_id, &payload, materialized);
    let trust = trust_for_attachment(payload.flavor, &provenance);
    let origin = format!("host_attachment:{}", payload.artifact_pointer);
    let ingress = crate::security::sanitize_ingress(&body, &origin, trust);
    let text = host_injection_envelope(
        "host_attachment",
        &payload.media_type,
        &injection_id,
        &ingress.delivered,
    );
    let action = match rendered {
        AttachmentRendering::DescriptionPlusPointer => SanitizationAction::Summarized,
        AttachmentRendering::PointerOnly => SanitizationAction::Pointerized,
        AttachmentRendering::ImageBlock | AttachmentRendering::InlineText => {
            SanitizationAction::Passed
        }
    };
    let sanitization = sanitization_verdict(
        trust,
        &serde_json::json!({
            "artifact_pointer": payload.artifact_pointer,
            "sha256": payload.sha256,
            "size_bytes": payload.size_bytes,
            "description": payload.description,
        }),
        &text,
        action,
        &ingress,
        payload.description_model.as_deref(),
    );
    record_host_ingress(session_id, &origin, &injection_id, trust, &body, &ingress);
    let metadata = serde_json::json!({
        "injection_id": injection_id,
        "sequence": sequence,
        "provenance": provenance,
        "sanitization": sanitization,
        "artifact_pointer": payload.artifact_pointer,
        "sha256": payload.sha256,
        "media_type": payload.media_type,
        "flavor": payload.flavor,
        "rendered": rendered,
        "description": payload.description,
        "description_model": payload.description_model,
    });
    let message = host_injection_user_message(&injection_id, &text, &metadata, image);
    let transcript_event = crate::llm::helpers::transcript_event(
        "host_attachment",
        "user",
        "public",
        &text,
        Some(metadata),
    );
    let event = AgentEvent::HostAttachment {
        session_id: session_id.to_string(),
        injection_id,
        media_type: payload.media_type,
        flavor: payload.flavor,
        artifact_pointer: payload.artifact_pointer,
        sha256: payload.sha256,
        size_bytes: payload.size_bytes,
        rendered,
        description: payload.description,
        description_model: payload.description_model,
        delivery,
        delivered_at_seam: Some(delivered_at_seam.to_string()),
        sequence,
        provenance,
        sanitization,
    };
    (message, transcript_event, event)
}

fn inject_typed_message(
    id: &str,
    message: VmValue,
    transcript_event: VmValue,
    agent_event: AgentEvent,
) -> Result<(), String> {
    let Some(msg_dict) = message.as_dict().cloned() else {
        return Err("agent_inject_host_event: materialized message must be a dict".into());
    };
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!(
                "agent_inject_host_event: unknown session id '{id}'"
            ));
        };
        let dict = state
            .transcript
            .as_dict()
            .cloned()
            .unwrap_or_else(crate::value::DictMap::new);
        let mut messages: Vec<VmValue> = match dict.get("messages") {
            Some(VmValue::List(list)) => list.iter().cloned().collect(),
            _ => Vec::new(),
        };
        let mut events: Vec<VmValue> = match dict.get("events") {
            Some(VmValue::List(list)) => list.iter().cloned().collect(),
            _ => crate::llm::helpers::transcript_events_from_messages(&messages),
        };
        let new_message = VmValue::dict(msg_dict);
        let message_index = messages.len();
        let journal_event = transcript_event.clone();
        events.push(transcript_event);
        messages.push(new_message);
        let mut next = dict;
        next.insert(
            crate::value::intern_key("events"),
            VmValue::List(std::sync::Arc::new(events)),
        );
        next.insert(
            crate::value::intern_key("messages"),
            VmValue::List(std::sync::Arc::new(messages)),
        );
        let persisted_message = next
            .get("messages")
            .and_then(|value| match value {
                VmValue::List(list) => list.get(message_index).cloned(),
                _ => None,
            })
            .unwrap_or(VmValue::Nil);
        apply_transcript_with_budget(state, VmValue::dict(next), "inject_host_event")?;
        crate::agent_session_journal::enqueue_message(
            &mut state.transcript_journal,
            crate::llm::helpers::vm_value_to_json(&journal_event),
            crate::llm::helpers::vm_value_to_json(&persisted_message),
        );
        emit_identified_user_message_event(id, &persisted_message);
        emit_llm_message_event(id, message_index, &persisted_message);
        crate::agent_events::emit_event(&agent_event);
        Ok(())
    })
}

fn host_injection_user_message(
    injection_id: &str,
    text: &str,
    metadata: &serde_json::Value,
    image: Option<crate::host_attachments::MaterializedAttachment>,
) -> VmValue {
    let mut message = BTreeMap::new();
    message.put_str("role", "user");
    message.put_str(
        "messageId",
        format!("hostinj_{}", injection_id.replace('-', "")),
    );
    let mut content = vec![serde_json::json!({"type": "text", "text": text})];
    match image {
        Some(crate::host_attachments::MaterializedAttachment::ImageUrl(url)) => {
            content.push(serde_json::json!({"type": "image", "url": url}));
        }
        Some(crate::host_attachments::MaterializedAttachment::ImageBase64 { media_type, data }) => {
            content.push(
                serde_json::json!({"type": "image", "base64": data, "media_type": media_type}),
            );
        }
        Some(crate::host_attachments::MaterializedAttachment::Text(_)) | None => {}
    }
    message.insert(
        "content".to_string(),
        crate::stdlib::json_to_vm_value(&serde_json::Value::Array(content)),
    );
    message.insert(
        "metadata".to_string(),
        crate::stdlib::json_to_vm_value(&serde_json::json!({
            "host_injection": metadata,
        })),
    );
    VmValue::dict(message)
}

fn host_tool_result_body(payload: &HostToolResultPayload) -> String {
    if let Some(output) = payload.raw_output.as_ref() {
        if let Some(text) = output.as_str() {
            return text.to_string();
        }
        return serde_json::to_string_pretty(output).unwrap_or_else(|_| output.to_string());
    }
    if let Some(pointer) = payload.result_pointer.as_deref() {
        return format!("Result artifact: {pointer}");
    }
    if let Some(error) = payload.error.as_deref() {
        return format!("Error: {error}");
    }
    String::new()
}

fn select_attachment_rendering(
    session_id: &str,
    payload: &HostAttachmentPayload,
    materialized: Result<crate::host_attachments::MaterializedAttachment, String>,
) -> (
    AttachmentRendering,
    String,
    Option<crate::host_attachments::MaterializedAttachment>,
) {
    if let Ok(crate::host_attachments::MaterializedAttachment::Text(text)) = &materialized {
        return (AttachmentRendering::InlineText, text.clone(), None);
    }
    let vision_capable = pinned_model(session_id)
        .map(|selector| crate::llm_config::resolve_model_info(&selector))
        .map(|resolved| {
            crate::llm::capabilities::lookup(&resolved.provider, &resolved.id).vision_supported
        })
        .unwrap_or(false);
    if vision_capable {
        if let Ok(
            image @ (crate::host_attachments::MaterializedAttachment::ImageUrl(_)
            | crate::host_attachments::MaterializedAttachment::ImageBase64 { .. }),
        ) = materialized
        {
            return (
                AttachmentRendering::ImageBlock,
                format!("Attachment: {}", payload.artifact_pointer),
                Some(image),
            );
        }
    }
    match payload
        .description
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        Some(description) => (
            AttachmentRendering::DescriptionPlusPointer,
            format!("{description}\nArtifact: {}", payload.artifact_pointer),
            None,
        ),
        None => (
            AttachmentRendering::PointerOnly,
            format!("Attachment: {}", payload.artifact_pointer),
            None,
        ),
    }
}

fn host_injection_envelope(kind: &str, subject: &str, injection_id: &str, body: &str) -> String {
    format!(
        "<{kind} subject=\"{}\" injection_id=\"{}\">\n{}\n</{kind}>",
        escape_attr(subject),
        escape_attr(injection_id),
        body
    )
}

fn escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn sanitization_verdict(
    trust: TrustLevel,
    original: &serde_json::Value,
    delivered: &str,
    action: SanitizationAction,
    ingress: &crate::security::SanitizedIngress,
    summary_model: Option<&str>,
) -> SanitizationVerdict {
    SanitizationVerdict {
        trust,
        detector: ingress.detector.clone(),
        action,
        original_bytes: serde_json::to_vec(original)
            .map(|bytes| bytes.len() as u64)
            .unwrap_or(0),
        delivered_bytes: delivered.len() as u64,
        summary_model: summary_model.map(str::to_owned),
        labels: ingress.labels.clone(),
    }
}

fn record_host_ingress(
    session_id: &str,
    origin: &str,
    introduced_by: &str,
    trust: TrustLevel,
    raw: &str,
    ingress: &crate::security::SanitizedIngress,
) {
    if !trust.is_untrusted() || raw.is_empty() {
        return;
    }
    push_session_taint(
        session_id,
        crate::security::TaintRecord {
            origin: origin.to_string(),
            trust,
            introduced_by: introduced_by.to_string(),
            detector: ingress.detector.clone(),
            labels: ingress.labels.clone(),
            endpoints: ingress.endpoints.clone(),
        },
    );
}

fn trust_for_tool(kind: Option<ToolKind>, provenance: &HostInjectionProvenance) -> TrustLevel {
    if provenance.source == "user_attachment" {
        return TrustLevel::Untrusted;
    }
    match kind {
        Some(ToolKind::Fetch) | Some(ToolKind::Search) => TrustLevel::Untrusted,
        Some(ToolKind::Read) => TrustLevel::SemiTrusted,
        Some(ToolKind::Execute)
        | Some(ToolKind::Edit)
        | Some(ToolKind::Delete)
        | Some(ToolKind::Move) => TrustLevel::SemiTrusted,
        _ => TrustLevel::Trusted,
    }
}

fn trust_for_attachment(
    flavor: AttachmentFlavor,
    provenance: &HostInjectionProvenance,
) -> TrustLevel {
    if provenance.source == "user_attachment" {
        return TrustLevel::Untrusted;
    }
    match flavor {
        AttachmentFlavor::TextFrame | AttachmentFlavor::FrameRing => TrustLevel::SemiTrusted,
        AttachmentFlavor::Image | AttachmentFlavor::File => TrustLevel::Untrusted,
    }
}

pub(super) fn emit_identified_user_message_event(session_id: &str, message: &VmValue) {
    let message_json = crate::llm::helpers::vm_value_to_json(message);
    let role = message_json.get("role").and_then(|value| value.as_str());
    if role != Some("user") {
        return;
    }
    let Some(message_id) = message_json
        .get("messageId")
        .or_else(|| message_json.get("message_id"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
    else {
        return;
    };
    let content = message_json
        .get("content")
        .map(user_message_content_blocks)
        .unwrap_or_default();
    crate::agent_events::emit_event(&crate::agent_events::AgentEvent::UserMessage {
        session_id: session_id.to_string(),
        message_id: message_id.to_string(),
        content,
    });
}

fn user_message_content_blocks(content: &serde_json::Value) -> Vec<serde_json::Value> {
    match content {
        serde_json::Value::Array(items) => items.clone(),
        serde_json::Value::String(text) => vec![serde_json::json!({
            "type": "text",
            "text": text,
        })],
        other => vec![serde_json::json!({
            "type": "text",
            "text": other.to_string(),
        })],
    }
}

pub(super) fn emit_llm_message_event(session_id: &str, message_index: usize, message: &VmValue) {
    let mut fields = serde_json::Map::new();
    fields.insert(
        "session_id".to_string(),
        serde_json::Value::String(session_id.to_string()),
    );
    fields.insert(
        "message_index".to_string(),
        serde_json::json!(message_index),
    );
    let message_json = crate::llm::helpers::vm_value_to_json(message);
    if let Some(role) = message_json.get("role").and_then(|value| value.as_str()) {
        fields.insert(
            "role".to_string(),
            serde_json::Value::String(role.to_string()),
        );
    }
    if let Some(content) = message_json.get("content") {
        fields.insert("content".to_string(), content.clone());
    }
    fields.insert("message".to_string(), message_json);
    crate::llm::append_observability_sidecar_entry("message", fields);
}
