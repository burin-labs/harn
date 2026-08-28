//! Typed provenance for the exact ordered message array served to one LLM call.
//!
//! Projection and visible-message assembly attach this private metadata while
//! they still own source indices and directive placement. Option extraction
//! removes it from provider-visible messages and carries the typed manifest to
//! the request observer through [`crate::llm::api::LlmCallOptions`].

use serde::{Deserialize, Serialize};

pub(crate) const MESSAGE_LINEAGE_KEY: &str = "_harn_message_lineage";

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ProjectionLineage {
    pub policy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix_hash: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MessageSemanticKind {
    User,
    Assistant,
    AssistantToolCall,
    ToolResult,
    Instruction,
    ContextDirective,
    CondensedMemory,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct MessageLineageEntry {
    pub semantic_kind: MessageSemanticKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_message_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_receipt_ref: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct MessageLineageManifest {
    pub projection: ProjectionLineage,
    pub messages: Vec<MessageLineageEntry>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct AttachedMessageLineage {
    pub projection: ProjectionLineage,
    #[serde(flatten)]
    pub message: MessageLineageEntry,
}

pub(crate) fn take_from_messages(
    messages: &mut [serde_json::Value],
) -> Option<MessageLineageManifest> {
    let attached = messages
        .iter_mut()
        .map(|message| {
            message
                .as_object_mut()
                .and_then(|object| object.remove(MESSAGE_LINEAGE_KEY))
                .and_then(|metadata| serde_json::from_value(metadata).ok())
        })
        .collect::<Vec<Option<AttachedMessageLineage>>>()
        .into_iter()
        .collect::<Option<Vec<AttachedMessageLineage>>>()?;
    let mut projection = None;
    let mut entries = Vec::with_capacity(attached.len());
    for attached in attached {
        match &projection {
            Some(existing) if existing != &attached.projection => return None,
            None => projection = Some(attached.projection.clone()),
            _ => {}
        }
        entries.push(attached.message);
    }
    Some(MessageLineageManifest {
        projection: projection.unwrap_or_else(raw_projection),
        messages: entries,
    })
}

pub(crate) fn raw_projection() -> ProjectionLineage {
    ProjectionLineage {
        policy: "raw".to_string(),
        event_ref: None,
        prefix_hash: None,
    }
}

pub(crate) fn attach_projection(
    messages: &mut [serde_json::Value],
    source_indices: &[Option<usize>],
    projection: &ProjectionLineage,
) {
    for (position, message) in messages.iter_mut().enumerate() {
        let Some(object) = message.as_object_mut() else {
            continue;
        };
        let attached = AttachedMessageLineage {
            projection: projection.clone(),
            message: MessageLineageEntry {
                source_message_index: source_indices.get(position).copied().flatten(),
                ..MessageLineageEntry::default()
            },
        };
        object.insert(
            MESSAGE_LINEAGE_KEY.to_string(),
            serde_json::to_value(attached).expect("message lineage is JSON representable"),
        );
    }
}
