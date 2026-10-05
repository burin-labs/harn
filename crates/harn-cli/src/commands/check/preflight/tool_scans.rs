//! Literal `tool_define` and `spawn_agent` config checks.

use super::*;

/// Validate `tool_define(reg, name, desc, {executor: ..., ...})` calls.
/// When the declared executor is `"host_bridge"`, the bound
/// `host_capability` is checked against the same capability map the
/// `host_call(...)` preflight uses; unknown bindings produce a tagged
/// diagnostic so projects can suppress via `[check].preflight_allow`.
///
/// All checks here are best-effort: a non-literal config dict (e.g. a
/// variable reference or a builder helper) silently skips. This
/// matches the broader preflight philosophy — only static literals
/// produce diagnostics.
pub(super) fn scan_tool_define_preflight(
    node: &SNode,
    args: &[SNode],
    host_capabilities: &HostCapabilities,
    file_path: &Path,
    source: &str,
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    let Some(config_arg) = args.get(3) else {
        return;
    };
    let Some(executor_node) = dict_literal_field(config_arg, "executor") else {
        return;
    };
    let Some(executor) = literal_string(executor_node) else {
        return;
    };
    let tool_name = args
        .get(1)
        .and_then(literal_string)
        .unwrap_or_else(|| "<dynamic>".to_string());

    if executor != "harn"
        && executor != "harn_builtin"
        && executor != "host_bridge"
        && executor != "mcp_server"
        && executor != "provider_native"
    {
        diagnostics.push(PreflightDiagnostic {
            code: Code::ToolDefinitionInvalid,
            path: file_path.display().to_string(),
            source: source.to_string(),
            span: executor_node.span,
            message: format!(
                "preflight: tool '{tool_name}' declares unknown executor \"{executor}\""
            ),
            help: Some(
                "expected one of: \"harn\", \"host_bridge\", \"mcp_server\", \"provider_native\""
                    .to_string(),
            ),
            tags: None,
        });
        return;
    }

    if executor != "host_bridge" {
        return;
    }
    let Some(capability_node) = dict_literal_field(config_arg, "host_capability") else {
        diagnostics.push(PreflightDiagnostic {
            code: Code::CapabilityBindingInvalid,
            path: file_path.display().to_string(),
            source: source.to_string(),
            span: node.span,
            message: format!(
                "preflight: tool '{tool_name}' declares executor: \"host_bridge\" \
                 but no `host_capability` binding"
            ),
            help: Some(
                "set host_capability to the canonical bridge identifier (e.g. \"interaction.ask\") \
                 so the binding can be validated against the host capability manifest"
                    .to_string(),
            ),
            tags: None,
        });
        return;
    };
    let Some(capability) = literal_string(capability_node) else {
        return;
    };
    let Some((cap, op)) = capability.split_once('.') else {
        diagnostics.push(PreflightDiagnostic {
            code: Code::CapabilityBindingInvalid,
            path: file_path.display().to_string(),
            source: source.to_string(),
            span: capability_node.span,
            message: format!(
                "preflight: tool '{tool_name}' has invalid host_capability \"{capability}\" \
                 (expected \"capability.operation\")"
            ),
            help: Some(
                "use the canonical \"capability.operation\" form so harn check can \
                 match it against host capability declarations"
                    .to_string(),
            ),
            tags: None,
        });
        return;
    };
    if !is_known_host_operation(host_capabilities, cap, op) {
        diagnostics.push(PreflightDiagnostic {
            code: Code::CapabilityUnknownOperation,
            path: file_path.display().to_string(),
            source: source.to_string(),
            span: capability_node.span,
            message: format!(
                "preflight: tool '{tool_name}' binds host_capability '{cap}.{op}' \
                 which is not declared by the host"
            ),
            help: Some(
                "declare the capability in [check].host_capabilities or \
                 [check].host_capabilities_path, or suppress via [check].preflight_allow"
                    .to_string(),
            ),
            tags: Some(format!("{cap}.{op}")),
        });
    }
}

pub(in super::super) fn dict_literal_field<'a>(node: &'a SNode, field: &str) -> Option<&'a SNode> {
    let Node::DictLiteral(entries) = &node.node else {
        return None;
    };
    entries.iter().find_map(|entry| match &entry.key.node {
        Node::Identifier(key) | Node::StringLiteral(key) if key == field => Some(&entry.value),
        _ => None,
    })
}

pub(super) fn scan_spawn_agent_preflight(
    config: &SNode,
    file_path: &Path,
    source: &str,
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    let Some(execution) = dict_literal_field(config, "execution") else {
        return;
    };
    if let Some(cwd) = dict_literal_field(execution, "cwd").and_then(literal_string) {
        let resolved = resolve_source_relative(file_path, &cwd);
        if !super::super::result_cache::probe_is_dir(&resolved) {
            diagnostics.push(PreflightDiagnostic {
                code: Code::ExecutionTargetMissing,
                path: file_path.display().to_string(),
                source: source.to_string(),
                span: execution.span,
                message: format!(
                    "preflight: worker execution cwd '{}' does not exist at {}",
                    cwd,
                    resolved.display()
                ),
                help: Some(
                    "keep literal worker cwd paths source-relative and valid, or switch to a worktree adapter"
                        .to_string(),
                ),
                tags: None,
            });
        }
    }
    let Some(worktree) = dict_literal_field(execution, "worktree") else {
        return;
    };
    if let Some(repo) = dict_literal_field(worktree, "repo").and_then(literal_string) {
        let resolved = resolve_source_relative(file_path, &repo);
        if !super::super::result_cache::probe_is_dir(&resolved) {
            diagnostics.push(PreflightDiagnostic {
                code: Code::ExecutionTargetMissing,
                path: file_path.display().to_string(),
                source: source.to_string(),
                span: worktree.span,
                message: format!(
                    "preflight: worker worktree repo '{}' does not exist at {}",
                    repo,
                    resolved.display()
                ),
                help: Some(
                    "point worktree.repo at a real git checkout so isolated execution can be prepared"
                        .to_string(),
                ),
                tags: None,
            });
        }
    }
}
