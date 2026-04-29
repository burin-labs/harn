//! Local (VM-side) handlers for read-only tools.
//!
//! `handle_tool_locally` short-circuits trivial reads (read_file, pwd, ls, ...)
//! without bridging to the host, reducing latency and avoiding split-brain
//! for passive operations.

pub(super) fn coerce_integer_like_tool_args(value: &mut serde_json::Value) {
    const INTEGER_KEYS: &[&str] = &[
        "range_start",
        "range_end",
        "offset",
        "limit",
        "timeout",
        "line",
        "start_line",
        "end_line",
        "count",
    ];

    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if INTEGER_KEYS.contains(&key.as_str()) {
                    if let Some(raw) = child.as_str() {
                        if let Ok(parsed) = raw.trim().parse::<i64>() {
                            *child = serde_json::json!(parsed);
                            continue;
                        }
                    }
                }
                coerce_integer_like_tool_args(child);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                coerce_integer_like_tool_args(item);
            }
        }
        _ => {}
    }
}

pub(super) fn resolve_local_tool_path(path: &str) -> std::path::PathBuf {
    let candidate = std::path::PathBuf::from(path);
    if candidate.is_absolute() {
        return candidate;
    }
    if let Some(cwd) =
        crate::stdlib::process::current_execution_context().and_then(|context| context.cwd)
    {
        return std::path::PathBuf::from(cwd).join(candidate);
    }
    crate::stdlib::process::resolve_source_relative_path(path)
}

/// Tool names that the VM-stdlib short-circuit (`handle_tool_locally`)
/// services without delegating to a registered handler or the host
/// bridge. The agent_loop pre-flight validator (harn#743) consults this
/// list so `executor: "harn"` declarations on these names are accepted
/// even without a registered handler closure — `handle_tool_locally`
/// provides the implicit Harn-side handler.
pub(crate) const VM_STDLIB_SHORT_CIRCUIT_TOOLS: &[&str] = &[
    "read_file",
    "list_directory",
    crate::llm::plan::EMIT_PLAN_TOOL,
    crate::llm::plan::UPDATE_PLAN_TOOL,
];

/// Returns `true` when `name` is a tool the VM stdlib services
/// directly via `handle_tool_locally`. Kept in lockstep with the match
/// arms below — adding a new arm without updating this list would
/// resurrect the harn#743 foot-gun.
pub(crate) fn is_vm_stdlib_short_circuit(name: &str) -> bool {
    VM_STDLIB_SHORT_CIRCUIT_TOOLS.contains(&name)
}

/// Handle read-only tools locally in the VM without bridging to the host.
/// This reduces latency and split-brain for passive operations.
pub(crate) fn handle_tool_locally(name: &str, args: &serde_json::Value) -> Option<String> {
    match name {
        crate::llm::plan::EMIT_PLAN_TOOL | crate::llm::plan::UPDATE_PLAN_TOOL => {
            let plan = crate::llm::plan::normalize_plan_tool_call(name, args);
            Some(serde_json::to_string_pretty(&plan).unwrap_or_else(|_| {
                "{\"_type\":\"plan_artifact\",\"schema_version\":\"harn.plan.v1\"}".to_string()
            }))
        }
        "read_file" => {
            let path = args
                .get("path")
                .or_else(|| args.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if path.is_empty() {
                return Some("Error: missing path parameter".to_string());
            }
            let resolved = resolve_local_tool_path(path);
            if resolved.is_dir() {
                return match std::fs::read_dir(&resolved) {
                    Ok(entries) => {
                        let mut names: Vec<String> = entries
                            .filter_map(|e| e.ok())
                            .map(|e| {
                                let name = e.file_name().to_string_lossy().into_owned();
                                if e.path().is_dir() {
                                    format!("{}/", name)
                                } else {
                                    name
                                }
                            })
                            .collect();
                        names.sort();
                        Some(names.join("\n"))
                    }
                    Err(e) => Some(format!("Error: cannot list directory '{}': {}", path, e)),
                };
            }
            let offset = args
                .get("offset")
                .and_then(|v| v.as_i64())
                .map(|v| v.max(1) as usize)
                .unwrap_or(1);
            let limit = args
                .get("limit")
                .and_then(|v| v.as_i64())
                .map(|v| v.clamp(1, 2000) as usize)
                .unwrap_or(2000);
            match std::fs::read_to_string(&resolved) {
                Ok(content) => {
                    let lines: Vec<&str> = content.lines().collect();
                    let total_lines = lines.len();
                    let start_idx = (offset - 1).min(total_lines);
                    let end_idx = (start_idx + limit).min(total_lines);
                    let mut numbered: String = lines[start_idx..end_idx]
                        .iter()
                        .enumerate()
                        .map(|(i, line)| format!("{}\t{}", start_idx + i + 1, line))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if end_idx < total_lines {
                        numbered.push_str(&format!(
                            "\n\n[... {} more lines not shown. Use offset={} to continue reading]",
                            total_lines - end_idx,
                            end_idx + 1
                        ));
                    }
                    Some(numbered)
                }
                Err(e) => Some(format!("Error: cannot read file '{}': {}", path, e)),
            }
        }
        "list_directory" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let resolved = resolve_local_tool_path(path);
            match std::fs::read_dir(&resolved) {
                Ok(entries) => {
                    let mut names: Vec<String> = entries
                        .filter_map(|e| e.ok())
                        .map(|e| {
                            let name = e.file_name().to_string_lossy().into_owned();
                            if e.path().is_dir() {
                                format!("{}/", name)
                            } else {
                                name
                            }
                        })
                        .collect();
                    names.sort();
                    Some(names.join("\n"))
                }
                Err(e) => Some(format!("Error: cannot list directory '{}': {}", path, e)),
            }
        }
        _ => None,
    }
}
