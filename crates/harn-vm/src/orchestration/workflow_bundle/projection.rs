//! Human-facing projections of a workflow bundle: the editable-field
//! descriptors an editor binds to, and the Mermaid rendering of an exported
//! graph.
//!
//! These are presentation of an already-validated bundle, not part of its
//! identity. Keeping them out of `mod.rs` leaves that file to the bundle's
//! contract — schema, hashing, signature, and validation — so a change to how
//! a graph is drawn cannot reach the bytes a signature covers.

use sha2::{Digest, Sha256};

use super::{
    ConnectorRequirement, WorkflowBundleEditableField, WorkflowBundleGraphEdge,
    WorkflowBundleGraphNode, WorkflowBundleTrigger,
};

fn editable_field(
    id: impl Into<String>,
    label: impl Into<String>,
    json_pointer: impl Into<String>,
    value_type: impl Into<String>,
    required: bool,
    enum_values: &[&str],
) -> WorkflowBundleEditableField {
    WorkflowBundleEditableField {
        id: id.into(),
        label: label.into(),
        json_pointer: json_pointer.into(),
        value_type: value_type.into(),
        required,
        enum_values: enum_values
            .iter()
            .map(|value| (*value).to_string())
            .collect(),
    }
}

fn json_pointer_segment(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

pub(super) fn trigger_editable_fields(
    index: usize,
    trigger: &WorkflowBundleTrigger,
) -> Vec<WorkflowBundleEditableField> {
    let base = format!("/triggers/{index}");
    let mut fields = vec![
        editable_field(
            format!("trigger.{}.kind", trigger.id),
            "Trigger kind",
            format!("{base}/kind"),
            "enum",
            true,
            &["github", "cron", "delay", "manual", "webhook", "mcp"],
        ),
        editable_field(
            format!("trigger.{}.node_id", trigger.id),
            "Target node",
            format!("{base}/node_id"),
            "string",
            false,
            &[],
        ),
    ];
    if trigger.provider.is_some() || trigger.kind == "github" {
        fields.push(editable_field(
            format!("trigger.{}.provider", trigger.id),
            "Provider",
            format!("{base}/provider"),
            "string",
            trigger.kind == "github",
            &[],
        ));
    }
    for (field, label, value_type) in [
        ("events", "Events", "list"),
        ("schedule", "Schedule", "string"),
        ("delay", "Delay", "string"),
        ("webhook_path", "Webhook path", "string"),
        ("mcp_tool", "MCP tool", "string"),
        ("resume_key", "Resume key", "string"),
        ("metadata", "Metadata", "object"),
    ] {
        fields.push(editable_field(
            format!("trigger.{}.{}", trigger.id, field),
            label,
            format!("{base}/{field}"),
            value_type,
            false,
            &[],
        ));
    }
    fields
}

pub(super) fn workflow_node_editable_fields(
    node_id: &str,
    capsule_id: Option<&String>,
) -> Vec<WorkflowBundleEditableField> {
    let escaped_node = json_pointer_segment(node_id);
    let mut fields = vec![
        editable_field(
            format!("workflow.{node_id}.task_label"),
            "Task label",
            format!("/workflow/nodes/{escaped_node}/task_label"),
            "string",
            false,
            &[],
        ),
        editable_field(
            format!("workflow.{node_id}.prompt"),
            "Prompt",
            format!("/workflow/nodes/{escaped_node}/prompt"),
            "string",
            false,
            &[],
        ),
        editable_field(
            format!("workflow.{node_id}.system"),
            "System prompt",
            format!("/workflow/nodes/{escaped_node}/system"),
            "string",
            false,
            &[],
        ),
        editable_field(
            format!("workflow.{node_id}.model_policy"),
            "Model policy",
            format!("/workflow/nodes/{escaped_node}/model_policy"),
            "object",
            false,
            &[],
        ),
        editable_field(
            format!("workflow.{node_id}.tools"),
            "Tool policy",
            format!("/workflow/nodes/{escaped_node}/tools"),
            "any",
            false,
            &[],
        ),
        editable_field(
            format!("workflow.{node_id}.capability_policy"),
            "Capability policy",
            format!("/workflow/nodes/{escaped_node}/capability_policy"),
            "object",
            false,
            &[],
        ),
        editable_field(
            format!("workflow.{node_id}.approval_policy"),
            "Approval policy",
            format!("/workflow/nodes/{escaped_node}/approval_policy"),
            "object",
            false,
            &[],
        ),
        editable_field(
            format!("workflow.{node_id}.retry_policy"),
            "Retry policy",
            format!("/workflow/nodes/{escaped_node}/retry_policy"),
            "object",
            false,
            &[],
        ),
    ];
    if let Some(capsule_id) = capsule_id {
        let escaped_capsule = json_pointer_segment(capsule_id);
        fields.extend([
            editable_field(
                format!("prompt_capsule.{capsule_id}.prompt"),
                "Prompt capsule",
                format!("/prompt_capsules/{escaped_capsule}/prompt"),
                "string",
                true,
                &[],
            ),
            editable_field(
                format!("prompt_capsule.{capsule_id}.system"),
                "Prompt capsule system",
                format!("/prompt_capsules/{escaped_capsule}/system"),
                "string",
                false,
                &[],
            ),
            editable_field(
                format!("prompt_capsule.{capsule_id}.context"),
                "Prompt capsule context",
                format!("/prompt_capsules/{escaped_capsule}/context"),
                "object",
                false,
                &[],
            ),
            editable_field(
                format!("prompt_capsule.{capsule_id}.trigger_id"),
                "Prompt capsule trigger",
                format!("/prompt_capsules/{escaped_capsule}/trigger_id"),
                "string",
                false,
                &[],
            ),
        ]);
    }
    fields
}

pub(super) fn connector_editable_fields(
    index: usize,
    connector: &ConnectorRequirement,
) -> Vec<WorkflowBundleEditableField> {
    let base = format!("/connectors/{index}");
    [
        ("id", "Connector id", "string", true),
        ("provider_id", "Provider id", "string", true),
        ("scopes", "Scopes", "list", false),
        ("setup_required", "Setup required", "bool", false),
        ("status_required", "Status required", "bool", false),
    ]
    .into_iter()
    .map(|(field, label, value_type, required)| {
        editable_field(
            format!("connector.{}.{}", connector.id, field),
            label,
            format!("{base}/{field}"),
            value_type,
            required,
            &[],
        )
    })
    .collect()
}

pub(super) fn retry_editable_fields() -> Vec<WorkflowBundleEditableField> {
    vec![
        editable_field(
            "policy.retry.max_attempts",
            "Retry attempts",
            "/policy/retry/max_attempts",
            "integer",
            true,
            &[],
        ),
        editable_field(
            "policy.retry.backoff",
            "Retry backoff",
            "/policy/retry/backoff",
            "string",
            true,
            &[],
        ),
    ]
}

pub(super) fn catchup_editable_fields() -> Vec<WorkflowBundleEditableField> {
    vec![
        editable_field(
            "policy.catchup.mode",
            "Catchup mode",
            "/policy/catchup/mode",
            "enum",
            true,
            &["none", "latest", "all"],
        ),
        editable_field(
            "policy.catchup.max_events",
            "Catchup max events",
            "/policy/catchup/max_events",
            "integer",
            false,
            &[],
        ),
    ]
}

pub(super) fn render_workflow_bundle_mermaid(
    nodes: &[WorkflowBundleGraphNode],
    edges: &[WorkflowBundleGraphEdge],
) -> String {
    let mut lines = vec!["flowchart TD".to_string()];
    for node in nodes {
        lines.push(format!(
            "  {}[\"{}\"]",
            mermaid_id(&node.id),
            mermaid_label(&format!("{}: {}", node.node_type, node.label))
        ));
    }
    for edge in edges {
        let label = edge
            .label
            .as_deref()
            .or(edge.branch.as_deref())
            .map(mermaid_label);
        match label {
            Some(label) if !label.is_empty() => lines.push(format!(
                "  {} -->|{}| {}",
                mermaid_id(&edge.from),
                label,
                mermaid_id(&edge.to)
            )),
            _ => lines.push(format!(
                "  {} --> {}",
                mermaid_id(&edge.from),
                mermaid_id(&edge.to)
            )),
        }
    }
    lines.join("\n")
}

fn mermaid_id(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let suffix = digest
        .iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let mut out = format!("n_{suffix}_");
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    out
}

fn mermaid_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', " ")
}
