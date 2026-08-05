use crate::value::VmValue;

fn entry_for<'a>(
    tools_val: Option<&'a VmValue>,
    tool_name: &str,
) -> Option<&'a crate::value::DictMap> {
    let VmValue::List(tools) = tools_val?.as_dict()?.get("tools")? else {
        return None;
    };
    tools.iter().find_map(|tool| {
        let entry = tool.as_dict()?;
        (entry.get("name").map(|value| value.display()).as_deref() == Some(tool_name))
            .then_some(entry)
    })
}

pub(super) fn annotations_for(
    tools_val: Option<&VmValue>,
    tool_name: &str,
) -> Option<crate::tool_annotations::ToolAnnotations> {
    entry_for(tools_val, tool_name)
        .and_then(|entry| entry.get("annotations"))
        .map(crate::llm::vm_value_to_json)
        .and_then(|value| serde_json::from_value(value).ok())
        .or_else(|| crate::orchestration::current_tool_annotations(tool_name))
}

pub(super) fn permission_context_for(
    tools_val: Option<&VmValue>,
    tool_name: &str,
) -> (
    Option<serde_json::Value>,
    Option<crate::tool_annotations::ToolAnnotations>,
) {
    (
        descriptor_for(tools_val, tool_name),
        annotations_for(tools_val, tool_name),
    )
}

/// Resolve a tool's model-visible descriptor plus its rug-pull flag so the
/// host can render the full tool text at approval time.
pub(super) fn descriptor_for(
    tools_val: Option<&VmValue>,
    tool_name: &str,
) -> Option<serde_json::Value> {
    let entry = entry_for(tools_val, tool_name)?;
    let mut out = serde_json::Map::new();
    for (source, target) in [
        ("description", "description"),
        ("inputSchema", "inputSchema"),
        ("_mcp_server", "mcpServer"),
    ] {
        if let Some(value) = entry.get(source) {
            out.insert(target.to_string(), crate::mcp::vm_value_to_serde(value));
        }
    }
    if matches!(entry.get("_schema_changed"), Some(VmValue::Bool(true))) {
        out.insert("schemaChanged".to_string(), serde_json::Value::Bool(true));
    }
    (!out.is_empty()).then_some(serde_json::Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_annotations::ToolKind;

    #[test]
    fn descriptor_and_annotations_come_from_the_dispatch_catalog() {
        let catalog = crate::json_to_vm_value(&serde_json::json!({
            "tools": [{
                "name": "linear__create",
                "description": "Create an issue",
                "_mcp_server": "linear",
                "_schema_changed": true,
                "annotations": {
                    "kind": "edit",
                    "arg_schema": {"path_params": ["path"]}
                }
            }]
        }));

        let descriptor = descriptor_for(Some(&catalog), "linear__create").expect("descriptor");
        assert_eq!(descriptor["description"], "Create an issue");
        assert_eq!(descriptor["mcpServer"], "linear");
        assert_eq!(descriptor["schemaChanged"], true);

        let annotations = annotations_for(Some(&catalog), "linear__create").expect("annotations");
        assert_eq!(annotations.kind, ToolKind::Edit);
        assert_eq!(annotations.arg_schema.path_params, ["path"]);

        assert!(descriptor_for(Some(&catalog), "unknown_tool").is_none());
        assert!(annotations_for(Some(&catalog), "unknown_tool").is_none());
    }
}
