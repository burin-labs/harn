use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};

use super::blocks::{
    default_visibility_for_role, normalize_message_blocks, overall_visibility, render_blocks_text,
};
use super::messages::json_messages_to_vm;
use super::{TRANSCRIPT_ASSET_TYPE, TRANSCRIPT_TYPE, TRANSCRIPT_VERSION};

pub(crate) fn transcript_message_list(
    transcript: &BTreeMap<String, VmValue>,
) -> Result<Vec<VmValue>, VmError> {
    match transcript.get("messages") {
        Some(VmValue::List(list)) => Ok((**list).clone()),
        Some(_) => Err(VmError::Thrown(VmValue::String(Rc::from(
            "transcript.messages must be a list",
        )))),
        None => Ok(Vec::new()),
    }
}

pub(crate) fn transcript_asset_list(
    transcript: &BTreeMap<String, VmValue>,
) -> Result<Vec<VmValue>, VmError> {
    match transcript.get("assets") {
        Some(VmValue::List(list)) => Ok((**list).clone()),
        Some(_) => Err(VmError::Thrown(VmValue::String(Rc::from(
            "transcript.assets must be a list",
        )))),
        None => Ok(Vec::new()),
    }
}

fn transcript_string_field(transcript: &BTreeMap<String, VmValue>, key: &str) -> Option<String> {
    transcript.get(key).and_then(|v| match v {
        VmValue::String(s) if !s.is_empty() => Some(s.to_string()),
        _ => None,
    })
}

pub(crate) fn transcript_summary_text(transcript: &BTreeMap<String, VmValue>) -> Option<String> {
    transcript_string_field(transcript, "summary")
}

pub(crate) fn transcript_id(transcript: &BTreeMap<String, VmValue>) -> Option<String> {
    transcript_string_field(transcript, "id")
}

pub(crate) fn new_transcript_with(
    id: Option<String>,
    messages: Vec<VmValue>,
    summary: Option<String>,
    metadata: Option<VmValue>,
) -> VmValue {
    new_transcript_with_events(
        id,
        messages,
        summary,
        metadata,
        Vec::new(),
        Vec::new(),
        None,
    )
}

pub(crate) fn new_transcript_with_events(
    id: Option<String>,
    messages: Vec<VmValue>,
    summary: Option<String>,
    metadata: Option<VmValue>,
    extra_events: Vec<VmValue>,
    assets: Vec<VmValue>,
    state: Option<&str>,
) -> VmValue {
    let mut transcript = BTreeMap::new();
    let mut events = transcript_events_from_messages(&messages);
    events.extend(extra_events);
    transcript.insert(
        "_type".to_string(),
        VmValue::String(Rc::from(TRANSCRIPT_TYPE)),
    );
    transcript.insert("version".to_string(), VmValue::Int(TRANSCRIPT_VERSION));
    transcript.insert(
        "id".to_string(),
        VmValue::String(Rc::from(
            id.unwrap_or_else(|| uuid::Uuid::now_v7().to_string()),
        )),
    );
    transcript.insert("messages".to_string(), VmValue::List(Rc::new(messages)));
    transcript.insert("events".to_string(), VmValue::List(Rc::new(events)));
    transcript.insert("assets".to_string(), VmValue::List(Rc::new(assets)));
    if let Some(summary) = summary {
        transcript.insert("summary".to_string(), VmValue::String(Rc::from(summary)));
    }
    if let Some(metadata) = metadata {
        transcript.insert("metadata".to_string(), metadata);
    }
    if let Some(state) = state {
        transcript.insert("state".to_string(), VmValue::String(Rc::from(state)));
    }
    VmValue::Dict(Rc::new(transcript))
}

fn transcript_event_from_message(message: &VmValue) -> VmValue {
    let dict = message.as_dict().cloned().unwrap_or_default();
    let role = dict
        .get("role")
        .map(|v| v.display())
        .unwrap_or_else(|| "user".to_string());
    let blocks = normalize_message_blocks(dict.get("content"), &role);
    let text = render_blocks_text(&blocks);
    let visibility = overall_visibility(&blocks, default_visibility_for_role(&role));
    let kind = if role == "tool_result" {
        "tool_result"
    } else {
        "message"
    };
    let mut event = BTreeMap::new();
    event.insert(
        "id".to_string(),
        VmValue::String(Rc::from(uuid::Uuid::now_v7().to_string())),
    );
    event.insert("kind".to_string(), VmValue::String(Rc::from(kind)));
    event.insert("role".to_string(), VmValue::String(Rc::from(role.as_str())));
    event.insert(
        "visibility".to_string(),
        VmValue::String(Rc::from(visibility)),
    );
    event.insert("text".to_string(), VmValue::String(Rc::from(text)));
    event.insert("blocks".to_string(), VmValue::List(Rc::new(blocks)));
    VmValue::Dict(Rc::new(event))
}

pub(crate) fn transcript_events_from_messages(messages: &[VmValue]) -> Vec<VmValue> {
    messages.iter().map(transcript_event_from_message).collect()
}

pub(crate) fn transcript_to_vm_with_events(
    id: Option<String>,
    summary: Option<String>,
    metadata: Option<serde_json::Value>,
    messages: &[serde_json::Value],
    extra_events: Vec<VmValue>,
    assets: Vec<VmValue>,
    state: Option<&str>,
) -> VmValue {
    let metadata_vm = metadata.as_ref().map(crate::stdlib::json_to_vm_value);
    new_transcript_with_events(
        id,
        json_messages_to_vm(messages),
        summary,
        metadata_vm,
        extra_events,
        assets,
        state,
    )
}

pub(crate) fn transcript_event(
    kind: &str,
    role: &str,
    visibility: &str,
    text: &str,
    metadata: Option<serde_json::Value>,
) -> VmValue {
    let mut event = BTreeMap::new();
    event.insert(
        "id".to_string(),
        VmValue::String(Rc::from(uuid::Uuid::now_v7().to_string())),
    );
    event.insert("kind".to_string(), VmValue::String(Rc::from(kind)));
    event.insert("role".to_string(), VmValue::String(Rc::from(role)));
    event.insert(
        "visibility".to_string(),
        VmValue::String(Rc::from(visibility)),
    );
    event.insert("text".to_string(), VmValue::String(Rc::from(text)));
    event.insert(
        "blocks".to_string(),
        VmValue::List(Rc::new(vec![VmValue::Dict(Rc::new(BTreeMap::from([
            ("type".to_string(), VmValue::String(Rc::from("text"))),
            ("text".to_string(), VmValue::String(Rc::from(text))),
            (
                "visibility".to_string(),
                VmValue::String(Rc::from(visibility)),
            ),
        ])))])),
    );
    if let Some(metadata) = metadata {
        event.insert(
            "metadata".to_string(),
            crate::stdlib::json_to_vm_value(&metadata),
        );
    }
    VmValue::Dict(Rc::new(event))
}

pub(crate) fn normalize_transcript_asset(value: &VmValue) -> VmValue {
    let mut asset = value.as_dict().cloned().unwrap_or_default();
    asset.insert(
        "_type".to_string(),
        VmValue::String(Rc::from(TRANSCRIPT_ASSET_TYPE)),
    );
    if !asset.contains_key("id") {
        asset.insert(
            "id".to_string(),
            VmValue::String(Rc::from(uuid::Uuid::now_v7().to_string())),
        );
    }
    if !asset.contains_key("kind") {
        asset.insert("kind".to_string(), VmValue::String(Rc::from("blob")));
    }
    if !asset.contains_key("visibility") {
        asset.insert(
            "visibility".to_string(),
            VmValue::String(Rc::from("internal")),
        );
    }
    if value.as_dict().is_none() {
        asset.insert(
            "storage".to_string(),
            VmValue::Dict(Rc::new(BTreeMap::from([(
                "path".to_string(),
                VmValue::String(Rc::from(value.display())),
            )]))),
        );
    }
    VmValue::Dict(Rc::new(asset))
}

pub(crate) fn is_transcript_value(value: &VmValue) -> bool {
    value
        .as_dict()
        .and_then(|d| d.get("_type"))
        .map(|v| v.display())
        .as_deref()
        == Some(TRANSCRIPT_TYPE)
}

