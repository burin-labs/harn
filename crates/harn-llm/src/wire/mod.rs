//! Provider request encoding with resolved policy and deferred audit delivery.

pub mod anthropic;
pub mod gemini;
pub mod openai;
pub mod openai_normalize;
pub mod prompt_cache;
pub mod responses;
pub mod thinking;

pub(super) fn sanitize_schema_for_provider(
    provider: &str,
    model: &str,
    profile: crate::schema_compat::SchemaCompatProfile,
    surface: crate::schema_compat::SchemaSurface,
    schema: &serde_json::Value,
    audits: &mut Vec<std::collections::BTreeMap<String, serde_json::Value>>,
) -> serde_json::Value {
    let projection =
        crate::schema_compat::sanitize_schema(provider, model, profile, surface, schema);
    if let Some(metadata) = projection.audit_metadata() {
        audits.push(metadata);
    }
    projection.schema
}

/// Merge a resolved reasoning-mode knob without replacing existing objects.
pub fn apply_reasoning_mode(
    body: &mut serde_json::Value,
    request: &harn_provider_catalog::ReasoningModeRequestDef,
) {
    let Some((leaf, parents)) = request.param_path.split_last() else {
        return;
    };
    let mut cursor = body;
    for segment in parents {
        if !cursor.is_object() {
            return;
        }
        cursor = cursor
            .as_object_mut()
            .expect("checked object above")
            .entry(segment.clone())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    }
    let Some(object) = cursor.as_object_mut() else {
        return;
    };
    object.insert(
        leaf.clone(),
        serde_json::Value::String(request.value.clone()),
    );
}

/// Encoded wire body and schema changes for the host's audit sink.
pub struct EncodedRequest {
    pub body: serde_json::Value,
    pub schema_audits: Vec<std::collections::BTreeMap<String, serde_json::Value>>,
}

/// Flatten only provider-visible text from a neutral message.
pub fn request_text_content(message: &serde_json::Value) -> String {
    let content = crate::content::provider_visible_content(&message["content"]);
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    let Some(parts) = content.as_array() else {
        return String::new();
    };
    let mut text = String::new();
    for part in parts {
        if let Some(value) = part.get("text").and_then(|value| value.as_str()) {
            text.push_str(value);
        } else if part.get("type").and_then(|value| value.as_str()) == Some("text") {
            if let Some(value) = part.get("text").and_then(|value| value.as_str()) {
                text.push_str(value);
            }
        }
    }
    text
}
