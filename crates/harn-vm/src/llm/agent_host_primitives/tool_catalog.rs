use crate::value::VmValue;

/// Normalize a connected MCP descriptor into the agent dispatch catalog.
pub(super) fn tag_mcp_tool(
    mut tool: serde_json::Value,
    server_name: &str,
    eager_schemas: bool,
) -> serde_json::Value {
    if let Some(obj) = tool.as_object_mut() {
        if let Some(metadata) = obj.get("_meta").cloned() {
            obj.insert("meta".into(), metadata);
        }
        let original_name = obj
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        obj.insert(
            "name".into(),
            serde_json::json!(format!("{server_name}__{original_name}")),
        );
        for (key, value) in [
            ("executor", "mcp_server"),
            ("mcp_server", server_name),
            ("_mcp_server", server_name),
            ("_mcp_tool_name", original_name.as_str()),
        ] {
            obj.insert(key.into(), serde_json::json!(value));
        }
        // Keep a server's explicit disclosure setting; defer by default.
        if !eager_schemas && !obj.contains_key("defer_loading") {
            obj.insert("defer_loading".into(), serde_json::Value::Bool(true));
        }
    }
    tool
}

pub(crate) fn entry_for<'a>(
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

/// The annotations dispatch enforces for `tool_name`: the registry entry's
/// own, else the ambient policy's. Argument normalization reads the same.
pub(crate) fn annotations_for(
    tools_val: Option<&VmValue>,
    tool_name: &str,
) -> Option<crate::tool_annotations::ToolAnnotations> {
    entry_for(tools_val, tool_name)
        .and_then(|entry| entry.get("annotations"))
        .map(crate::llm::vm_value_to_json)
        .and_then(|value| serde_json::from_value(value).ok())
        .or_else(|| crate::orchestration::current_tool_annotations(tool_name))
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
