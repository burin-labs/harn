use crate::value::VmValue;

/// Convert a VmValue returned by a prompt handler into MCP messages.
pub(super) fn prompt_value_to_messages(value: &VmValue) -> Vec<serde_json::Value> {
    match value {
        VmValue::String(s) => {
            vec![serde_json::json!({
                "role": "user",
                "content": { "type": "text", "text": &**s }
            })]
        }
        VmValue::List(items) => items
            .iter()
            .flat_map(|item| {
                if let VmValue::Dict(d) = item {
                    let role = d
                        .get("role")
                        .map(|v| v.display())
                        .unwrap_or_else(|| "user".into());
                    prompt_content_messages(&role, d.get("content"))
                } else {
                    vec![serde_json::json!({
                        "role": "user",
                        "content": { "type": "text", "text": item.display() }
                    })]
                }
            })
            .collect(),
        _ => {
            vec![serde_json::json!({
                "role": "user",
                "content": { "type": "text", "text": value.display() }
            })]
        }
    }
}

fn prompt_content_messages(role: &str, content: Option<&VmValue>) -> Vec<serde_json::Value> {
    match content {
        Some(value @ VmValue::Dict(_)) => vec![serde_json::json!({
            "role": role,
            "content": normalize_prompt_content(value),
        })],
        Some(VmValue::List(items)) => items
            .iter()
            .map(|item| {
                serde_json::json!({
                    "role": role,
                    "content": normalize_prompt_content(item),
                })
            })
            .collect(),
        Some(value) => vec![serde_json::json!({
            "role": role,
            "content": { "type": "text", "text": value.display() },
        })],
        None => vec![serde_json::json!({
            "role": role,
            "content": { "type": "text", "text": "" },
        })],
    }
}

fn normalize_prompt_content(value: &VmValue) -> serde_json::Value {
    if let VmValue::Dict(d) = value {
        let content_type = d.get("type").map(|v| v.display()).unwrap_or_default();
        match content_type.as_str() {
            "text" => serde_json::json!({
                "type": "text",
                "text": d.get("text").map(|v| v.display()).unwrap_or_default(),
            }),
            "image" => {
                let mut content = serde_json::json!({
                    "type": "image",
                    "data": d.get("data").map(|v| v.display()).unwrap_or_default(),
                    "mimeType": d
                        .get("mimeType")
                        .or_else(|| d.get("mime_type"))
                        .map(|v| v.display())
                        .unwrap_or_else(|| "application/octet-stream".to_string()),
                });
                if let Some(annotations) = d.get("annotations") {
                    content["annotations"] = vm_value_to_json(annotations);
                }
                content
            }
            "audio" => serde_json::json!({
                "type": "audio",
                "data": d.get("data").map(|v| v.display()).unwrap_or_default(),
                "mimeType": d
                    .get("mimeType")
                    .or_else(|| d.get("mime_type"))
                    .map(|v| v.display())
                    .unwrap_or_else(|| "application/octet-stream".to_string()),
            }),
            "resource" => {
                let mut content = serde_json::json!({ "type": "resource" });
                if let Some(resource) = d.get("resource") {
                    content["resource"] = vm_value_to_json(resource);
                }
                content
            }
            _ => serde_json::json!({
                "type": "text",
                "text": d.get("text").map(|v| v.display()).unwrap_or_else(|| value.display()),
            }),
        }
    } else {
        serde_json::json!({ "type": "text", "text": value.display() })
    }
}

/// Convert a tool result VmValue into MCP content items.
///
/// Supports text, embedded resource, and resource_link content types.
/// If the value is a list of dicts with a `type` field, each is treated as a
/// content item. Otherwise, the whole value is serialized as a single text item.
pub(super) fn vm_value_to_content(value: &VmValue) -> Vec<serde_json::Value> {
    if let VmValue::List(items) = value {
        let mut content = Vec::new();
        for item in items.iter() {
            if let VmValue::Dict(d) = item {
                let item_type = d.get("type").map(|v| v.display()).unwrap_or_default();
                match item_type.as_str() {
                    "resource" => {
                        let mut entry = serde_json::json!({ "type": "resource" });
                        if let Some(resource) = d.get("resource") {
                            entry["resource"] = vm_value_to_json(resource);
                        }
                        content.push(entry);
                    }
                    "resource_link" => {
                        let mut entry = serde_json::json!({ "type": "resource_link" });
                        if let Some(uri) = d.get("uri") {
                            entry["uri"] = serde_json::json!(uri.display());
                        }
                        if let Some(name) = d.get("name") {
                            entry["name"] = serde_json::json!(name.display());
                        }
                        if let Some(desc) = d.get("description") {
                            entry["description"] = serde_json::json!(desc.display());
                        }
                        if let Some(mime) = d.get("mimeType") {
                            entry["mimeType"] = serde_json::json!(mime.display());
                        }
                        content.push(entry);
                    }
                    _ => {
                        let text = d
                            .get("text")
                            .map(|v| v.display())
                            .unwrap_or_else(|| item.display());
                        content.push(serde_json::json!({ "type": "text", "text": text }));
                    }
                }
            } else {
                content.push(serde_json::json!({ "type": "text", "text": item.display() }));
            }
        }
        if content.is_empty() {
            vec![serde_json::json!({ "type": "text", "text": value.display() })]
        } else {
            content
        }
    } else {
        vec![serde_json::json!({ "type": "text", "text": value.display() })]
    }
}

/// Convert a VmValue to a serde_json::Value.
pub(super) fn vm_value_to_json(value: &VmValue) -> serde_json::Value {
    match value {
        VmValue::Nil => serde_json::Value::Null,
        VmValue::Bool(b) => serde_json::json!(b),
        VmValue::Int(n) => serde_json::json!(n),
        VmValue::Float(f) => serde_json::json!(f),
        VmValue::String(s) => serde_json::json!(&**s),
        VmValue::List(items) => {
            serde_json::Value::Array(items.iter().map(vm_value_to_json).collect())
        }
        VmValue::Dict(d) => {
            let mut map = serde_json::Map::new();
            for (k, v) in d.iter() {
                map.insert(k.clone(), vm_value_to_json(v));
            }
            serde_json::Value::Object(map)
        }
        VmValue::StructInstance { .. } => {
            let mut map = serde_json::Map::new();
            for (k, v) in value.struct_fields_map().unwrap_or_default().iter() {
                map.insert(k.clone(), vm_value_to_json(v));
            }
            serde_json::Value::Object(map)
        }
        _ => serde_json::json!(value.display()),
    }
}

/// Convert a VmValue annotations dict to a serde_json::Value with only the
/// recognized MCP annotation fields.
pub(super) fn annotations_to_json(annotations: &VmValue) -> Option<serde_json::Value> {
    let dict = match annotations {
        VmValue::Dict(d) => d,
        _ => return None,
    };

    let mut out = serde_json::Map::new();
    let str_keys = ["title"];
    let bool_keys = [
        "readOnlyHint",
        "destructiveHint",
        "idempotentHint",
        "openWorldHint",
    ];

    for key in str_keys {
        if let Some(VmValue::String(s)) = dict.get(key) {
            out.insert(key.into(), serde_json::json!(&**s));
        }
    }
    for key in bool_keys {
        if let Some(VmValue::Bool(b)) = dict.get(key) {
            out.insert(key.into(), serde_json::json!(b));
        }
    }

    if out.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(out))
    }
}
