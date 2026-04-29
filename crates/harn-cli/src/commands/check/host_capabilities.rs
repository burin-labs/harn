use std::collections::{HashMap, HashSet};

use crate::package::CheckConfig;

fn default_host_capabilities() -> HashMap<String, HashSet<String>> {
    HashMap::from([
        (
            "workspace".to_string(),
            HashSet::from([
                "read_text".to_string(),
                "write_text".to_string(),
                "apply_edit".to_string(),
                "delete".to_string(),
                "exists".to_string(),
                "file_exists".to_string(),
                "list".to_string(),
                "project_root".to_string(),
                "roots".to_string(),
            ]),
        ),
        (
            "process".to_string(),
            HashSet::from([
                "exec".to_string(),
                "get_default_shell".to_string(),
                "list_shells".to_string(),
                "set_default_shell".to_string(),
                "shell_invocation".to_string(),
            ]),
        ),
        (
            "template".to_string(),
            HashSet::from(["render".to_string()]),
        ),
        (
            "interaction".to_string(),
            HashSet::from(["ask".to_string()]),
        ),
        (
            "runtime".to_string(),
            HashSet::from([
                "approved_plan".to_string(),
                "dry_run".to_string(),
                "pipeline_input".to_string(),
                "record_run".to_string(),
                "set_result".to_string(),
                "task".to_string(),
            ]),
        ),
        (
            "project".to_string(),
            HashSet::from([
                "agent_instructions".to_string(),
                "code_patterns".to_string(),
                "compute_content_hash".to_string(),
                "ide_context".to_string(),
                "lessons".to_string(),
                "mcp_config".to_string(),
                "metadata_get".to_string(),
                "metadata_refresh_hashes".to_string(),
                "metadata_save".to_string(),
                "metadata_set".to_string(),
                "metadata_stale".to_string(),
                "scan".to_string(),
                "scope_test_command".to_string(),
                "test_commands".to_string(),
            ]),
        ),
        (
            "session".to_string(),
            HashSet::from([
                "active_roots".to_string(),
                "changed_paths".to_string(),
                "preread_get".to_string(),
                "preread_read_many".to_string(),
            ]),
        ),
        (
            "editor".to_string(),
            HashSet::from([
                "get_active_file".to_string(),
                "get_selection".to_string(),
                "get_visible_files".to_string(),
            ]),
        ),
        (
            "diagnostics".to_string(),
            HashSet::from(["get_causal_traces".to_string(), "get_errors".to_string()]),
        ),
        (
            "git".to_string(),
            HashSet::from(["get_branch".to_string(), "get_diff".to_string()]),
        ),
        (
            "learning".to_string(),
            HashSet::from([
                "get_learned_rules".to_string(),
                "report_correction".to_string(),
            ]),
        ),
    ])
}

fn merge_host_capability_map(
    target: &mut HashMap<String, HashSet<String>>,
    source: HashMap<String, HashSet<String>>,
) {
    for (capability, ops) in source {
        target.entry(capability).or_default().extend(ops);
    }
}

pub(super) fn parse_host_capability_value(
    value: &serde_json::Value,
) -> HashMap<String, HashSet<String>> {
    let root = value.get("capabilities").unwrap_or(value);
    let mut result = HashMap::new();
    let Some(capabilities) = root.as_object() else {
        return result;
    };
    for (capability, entry) in capabilities {
        let mut ops = HashSet::new();
        if let Some(list) = entry.as_array() {
            for item in list {
                if let Some(op) = item.as_str() {
                    ops.insert(op.to_string());
                }
            }
        } else if let Some(obj) = entry.as_object() {
            if let Some(list) = obj
                .get("operations")
                .or_else(|| obj.get("ops"))
                .and_then(|v| v.as_array())
            {
                for item in list {
                    if let Some(op) = item.as_str() {
                        ops.insert(op.to_string());
                    }
                }
            } else {
                for (op, enabled) in obj {
                    if enabled.as_bool().unwrap_or(true) {
                        ops.insert(op.to_string());
                    }
                }
            }
        }
        if !ops.is_empty() {
            result.insert(capability.to_string(), ops);
        }
    }
    result
}

pub(crate) fn load_host_capabilities(config: &CheckConfig) -> HashMap<String, HashSet<String>> {
    let mut capabilities = default_host_capabilities();
    let inline = config
        .host_capabilities
        .iter()
        .map(|(capability, ops)| {
            (
                capability.clone(),
                ops.iter().cloned().collect::<HashSet<String>>(),
            )
        })
        .collect::<HashMap<_, _>>();
    merge_host_capability_map(&mut capabilities, inline);
    if let Some(path) = config.host_capabilities_path.as_deref() {
        if let Ok(content) = std::fs::read_to_string(path) {
            let parsed_json = serde_json::from_str::<serde_json::Value>(&content).ok();
            let parsed_toml = toml::from_str::<toml::Value>(&content)
                .ok()
                .and_then(|value| serde_json::to_value(value).ok());
            if let Some(value) = parsed_json.or(parsed_toml) {
                merge_host_capability_map(&mut capabilities, parse_host_capability_value(&value));
            }
        }
    }
    capabilities
}

pub(super) fn is_known_host_operation(
    capabilities: &HashMap<String, HashSet<String>>,
    capability: &str,
    operation: &str,
) -> bool {
    capabilities
        .get(capability)
        .is_some_and(|ops| ops.contains(operation))
}
