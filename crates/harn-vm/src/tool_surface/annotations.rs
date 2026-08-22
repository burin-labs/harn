//! Decode the annotation projection accepted by tool-surface validation.

use std::collections::BTreeMap;

use crate::tool_annotations::{
    CompletionEvidenceRole, SideEffectLevel, ToolAnnotations, ToolArgSchema,
    ToolDependencyRangeParams, ToolKind,
};

fn parse_tool_kind(value: Option<&serde_json::Value>) -> ToolKind {
    match value.and_then(serde_json::Value::as_str).unwrap_or("") {
        "read" => ToolKind::Read,
        "edit" => ToolKind::Edit,
        "delete" => ToolKind::Delete,
        "move" => ToolKind::Move,
        "search" => ToolKind::Search,
        "execute" => ToolKind::Execute,
        "think" => ToolKind::Think,
        "fetch" => ToolKind::Fetch,
        _ => ToolKind::Other,
    }
}

pub(super) fn parse_tool_annotations(
    map: &serde_json::Map<String, serde_json::Value>,
) -> ToolAnnotations {
    let policy = map
        .get("policy")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();

    let capabilities = policy
        .get("capabilities")
        .and_then(serde_json::Value::as_object)
        .map(|caps| {
            caps.iter()
                .map(|(capability, ops)| {
                    let values = ops
                        .as_array()
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|item| item.as_str().map(ToOwned::to_owned))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    (capability.clone(), values)
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();

    let arg_schema = if let Some(schema) = policy.get("arg_schema") {
        serde_json::from_value::<ToolArgSchema>(schema.clone()).unwrap_or_default()
    } else {
        ToolArgSchema {
            path_params: string_array(&policy, "path_params"),
            dependency_key_params: string_array(&policy, "dependency_key_params"),
            dependency_range_params: policy
                .get("dependency_range_params")
                .and_then(|value| {
                    serde_json::from_value::<Vec<ToolDependencyRangeParams>>(value.clone()).ok()
                })
                .unwrap_or_default(),
            arg_aliases: policy
                .get("arg_aliases")
                .and_then(serde_json::Value::as_object)
                .map(|aliases| {
                    aliases
                        .iter()
                        .filter_map(|(key, value)| {
                            value.as_str().map(|alias| (key.clone(), alias.to_string()))
                        })
                        .collect::<BTreeMap<_, _>>()
                })
                .unwrap_or_default(),
            required: string_array(&policy, "required"),
        }
    };

    ToolAnnotations {
        kind: parse_tool_kind(policy.get("kind")),
        side_effect_level: policy
            .get("side_effect_level")
            .and_then(serde_json::Value::as_str)
            .map(SideEffectLevel::parse)
            .unwrap_or_default(),
        completion_evidence_role: policy
            .get("completion_evidence_role")
            .and_then(|value| serde_json::from_value::<CompletionEvidenceRole>(value.clone()).ok()),
        arg_schema,
        capabilities,
        emits_artifacts: bool_field(&policy, "emits_artifacts").unwrap_or(false),
        result_readers: string_array_from_value(
            policy
                .get("result_readers")
                .or_else(|| policy.get("readable_result_routes")),
        ),
        inline_result: bool_field(&policy, "inline_result").unwrap_or(false),
        read_only_hint: hint(map, &policy, "readOnlyHint"),
        destructive_hint: hint(map, &policy, "destructiveHint"),
        idempotent_hint: hint(map, &policy, "idempotentHint"),
        open_world_hint: hint(map, &policy, "openWorldHint"),
    }
}

fn string_array(map: &serde_json::Map<String, serde_json::Value>, field: &str) -> Vec<String> {
    string_array_from_value(map.get(field))
}

fn string_array_from_value(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn bool_field(map: &serde_json::Map<String, serde_json::Value>, field: &str) -> Option<bool> {
    map.get(field).and_then(serde_json::Value::as_bool)
}

fn hint(
    tool: &serde_json::Map<String, serde_json::Value>,
    policy: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Option<bool> {
    tool.get(field)
        .or_else(|| policy.get(field))
        .and_then(serde_json::Value::as_bool)
}
