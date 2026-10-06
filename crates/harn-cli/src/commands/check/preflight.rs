use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use harn_modules::resolve_import_path;
use harn_parser::{DiagnosticCode as Code, Node, SNode};

use super::host_capabilities::{is_known_host_operation, HostCapabilities};
use super::imports::{
    scan_import_collisions, scan_re_export_conflicts, scan_selective_import_visibility,
};
use super::mock_host::collect_mock_host_capabilities;
use super::source::parse_resolved_module;
use crate::package::CheckConfig;

mod effect_inheritance;
mod execution_target;
mod host_param_discriminators;
mod llm_composition;
mod static_tool_surface;
mod targets;
mod tool_scans;

use effect_inheritance::*;
use execution_target::{is_process_execution_method, scan_execution_dir_preflight};
use host_param_discriminators::scan_host_param_discriminators;
pub(super) use host_param_discriminators::{host_render_path_arg, parse_host_call_args};
use static_tool_surface::*;
use targets::*;
pub(super) use targets::{
    find_unique_basename, literal_string, resolve_preflight_target, resolve_source_relative,
};
pub(super) use tool_scans::dict_literal_field;
use tool_scans::*;

#[derive(Debug)]
pub(crate) struct PreflightDiagnostic {
    pub(crate) code: Code,
    pub(crate) path: String,
    pub(crate) source: String,
    pub(crate) span: harn_lexer::Span,
    pub(crate) message: String,
    pub(crate) help: Option<String>,
    /// Optional `"capability.operation"` tag for `[check].preflight_allow`.
    pub(crate) tags: Option<String>,
}

/// Returns whether `tag` matches an exact, capability-wide, or global allow entry.
pub(crate) fn is_preflight_allowed(tag: &Option<String>, allow: &[String]) -> bool {
    let Some(tag) = tag else { return false };
    let (cap, _) = tag.split_once('.').unwrap_or((tag.as_str(), ""));
    allow.iter().any(|entry| {
        let entry = entry.trim();
        if entry == "*" || entry == tag {
            return true;
        }
        if let Some(prefix) = entry.strip_suffix(".*") {
            return prefix == cap;
        }
        entry == cap
    })
}

pub(super) fn collect_preflight_diagnostics_with_host_capabilities(
    path: &Path,
    source: &str,
    program: &[SNode],
    config: &CheckConfig,
    module_graph: &harn_modules::ModuleGraph,
    configured_host_capabilities: &HostCapabilities,
) -> Vec<PreflightDiagnostic> {
    let mut diagnostics = Vec::new();
    let mut visited = HashSet::new();
    let canonical = harn_modules::canonical_path(path);
    let mut host_capabilities = configured_host_capabilities.clone();
    let mut mocked_caps_visited = HashSet::new();
    collect_mock_host_capabilities(
        &canonical,
        source,
        program,
        &mut mocked_caps_visited,
        host_capabilities.operations_mut(),
    );
    scan_program_preflight(
        &canonical,
        source,
        program,
        config,
        &host_capabilities,
        &mut visited,
        &mut diagnostics,
    );
    scan_import_collisions(&canonical, source, program, module_graph, &mut diagnostics);
    scan_selective_import_visibility(&canonical, source, module_graph, &mut diagnostics);
    scan_re_export_conflicts(&canonical, source, program, module_graph, &mut diagnostics);
    scan_static_tool_surface_preflight(&canonical, source, program, config, &mut diagnostics);
    llm_composition::scan_llm_capability_composition_preflight(
        &canonical,
        source,
        program,
        &mut diagnostics,
    );
    scan_effect_inheritance_preflight(&canonical, source, program, &mut diagnostics);
    diagnostics
}

fn scan_program_preflight(
    file_path: &Path,
    source: &str,
    program: &[SNode],
    config: &CheckConfig,
    host_capabilities: &HostCapabilities,
    visited: &mut HashSet<PathBuf>,
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    let canonical = harn_modules::canonical_path(file_path);
    if !visited.insert(canonical.clone()) {
        return;
    }
    for node in program {
        scan_node_preflight(
            node,
            &canonical,
            source,
            config,
            host_capabilities,
            visited,
            diagnostics,
        );
    }
}

fn scan_node_preflight(
    node: &SNode,
    file_path: &Path,
    source: &str,
    config: &CheckConfig,
    host_capabilities: &HostCapabilities,
    visited: &mut HashSet<PathBuf>,
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    match &node.node {
        Node::ImportDecl { path, .. }
        | Node::SelectiveImport { path, .. }
        | Node::NamespaceImport { path, .. } => match resolve_import_path(file_path, path) {
            Some(import_path) => {
                if let Some(parsed) = parse_resolved_module(&import_path) {
                    scan_program_preflight(
                        &import_path,
                        &parsed.0,
                        &parsed.1,
                        config,
                        host_capabilities,
                        visited,
                        diagnostics,
                    );
                }
            }
            None => diagnostics.push(PreflightDiagnostic {
                code: Code::ModuleImportUnresolved,
                path: file_path.display().to_string(),
                source: source.to_string(),
                span: node.span,
                message: format!("preflight: unresolved import '{path}'"),
                help: Some("verify the import path and packaged module layout".to_string()),
                tags: None,
            }),
        },
        Node::MethodCall { method, args, .. }
            if matches!(
                method.as_str(),
                "render_prompt" | "render_prompt_with_provenance"
            ) =>
        {
            if let Some(template_path) = args.first().and_then(literal_template_path) {
                if scan_stdlib_prompt_target(
                    &template_path,
                    &format!("harness.fs.{method}"),
                    args[0].span,
                    file_path,
                    source,
                    diagnostics,
                ) {
                    scan_children(
                        args,
                        file_path,
                        source,
                        config,
                        host_capabilities,
                        visited,
                        diagnostics,
                    );
                    return;
                }
                if let Some(asset_ref) = harn_modules::asset_paths::parse(&template_path) {
                    let anchor = file_path.parent().unwrap_or(Path::new("."));
                    if let Err(err) = harn_modules::asset_paths::resolve(&asset_ref, anchor) {
                        // Surface resolver errors (no project root, unknown
                        // alias) before the file-existence check so the
                        // user sees the structural cause, not a generic
                        // "render target does not exist" message.
                        diagnostics.push(PreflightDiagnostic {
                            code: Code::ImportResolutionFailed,
                            path: file_path.display().to_string(),
                            source: source.to_string(),
                            span: args[0].span,
                            message: format!("preflight: {err}"),
                            help: Some(
                                "see docs/src/modules.md#package-root-prompt-assets for `@/...` and `@<alias>/...` syntax".to_string(),
                            ),
                            tags: None,
                        });
                        return;
                    }
                }
                let resolved = resolve_preflight_target(file_path, &template_path, config);
                if let Some(existing) = resolved
                    .iter()
                    .find(|path| super::result_cache::probe_exists(path))
                {
                    if let Ok(body) = super::result_cache::probe_read_to_string(existing) {
                        if let Err(err) = harn_vm::stdlib::template::validate_template_syntax(&body)
                        {
                            diagnostics.push(PreflightDiagnostic {
                                code: Code::PromptTemplateParse,
                                path: file_path.display().to_string(),
                                source: source.to_string(),
                                span: args[0].span,
                                message: format!(
                                    "preflight: template '{template_path}' has a syntax error: {err}"
                                ),
                                help: Some(
                                    "see docs/src/prompt-templating.md for supported directives"
                                        .to_string(),
                                ),
                                tags: None,
                            });
                        }
                    }
                } else {
                    diagnostics.push(PreflightDiagnostic {
                        code: Code::PromptTargetMissing,
                        path: file_path.display().to_string(),
                        source: source.to_string(),
                        span: args[0].span,
                        message: format!(
                            "preflight: harness.fs.{method} target '{}' does not exist at {}",
                            template_path,
                            render_candidate_paths(&resolved)
                        ),
                        help: Some(render_target_miss_help(file_path, &template_path)),
                        tags: None,
                    });
                }
            }
        }
        Node::FunctionCall { name, args, .. } if name == "exec_at" || name == "shell_at" => {
            scan_execution_dir_preflight(args, file_path, source, diagnostics);
        }
        Node::FunctionCall { name, args, .. } if name == "spawn_agent" => {
            if let Some(agent_config) = args.last() {
                scan_spawn_agent_preflight(agent_config, file_path, source, diagnostics);
            }
            scan_children(
                args,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::FunctionCall { name, args, .. } if name == "host_invoke" => {
            diagnostics.push(PreflightDiagnostic {
                code: Code::DeprecatedStdlibSymbol,
                path: file_path.display().to_string(),
                source: source.to_string(),
                span: node.span,
                message: "preflight: host_invoke(...) was removed; use host_call(\"capability.operation\", args)".to_string(),
                help: Some(
                    "replace host_invoke(\"project\", \"scan\", {}) with host_call(\"project.scan\", {})"
                        .to_string(),
                ),
                tags: None,
            });
            scan_children(
                args,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::FunctionCall { name, args, .. } if name == "tool_define" => {
            // harn#743: when a tool declares `executor: "host_bridge"`,
            // its `host_capability` must point at a real host operation
            // — otherwise the model gets a tool whose dispatch will
            // fail at runtime with no static feedback today. Validate
            // the same capability map `host_call(...)` checks against
            // so the failure surfaces during `harn check`.
            scan_tool_define_preflight(
                node,
                args,
                host_capabilities,
                file_path,
                source,
                diagnostics,
            );
            scan_children(
                args,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::FunctionCall { name, args, .. } if name == "host_call" => {
            if let Some((cap, op, params_arg)) = parse_host_call_args(args) {
                if !is_known_host_operation(host_capabilities, &cap, &op) {
                    diagnostics.push(PreflightDiagnostic {
                        code: Code::CapabilityUnknownOperation,
                        path: file_path.display().to_string(),
                        source: source.to_string(),
                        span: node.span,
                        message: format!(
                            "preflight: unknown host capability/operation '{cap}.{op}'"
                        ),
                        help: Some(
                            "declare additional host capabilities in [check].host_capabilities, [check].host_capabilities_path, --host-capabilities, or suppress via [check].preflight_allow"
                                .to_string(),
                        ),
                        tags: Some(format!("{cap}.{op}")),
                    });
                } else {
                    scan_host_param_discriminators(
                        node,
                        params_arg,
                        host_capabilities,
                        &cap,
                        &op,
                        file_path,
                        source,
                        diagnostics,
                    );
                }
                if cap == "template" && op == "render" {
                    if let Some(template_path) = host_render_path_arg(params_arg) {
                        if !scan_stdlib_prompt_target(
                            &template_path,
                            "host template render",
                            params_arg.map(|arg| arg.span).unwrap_or(node.span),
                            file_path,
                            source,
                            diagnostics,
                        ) {
                            if let Some(asset_ref) =
                                harn_modules::asset_paths::parse(&template_path)
                            {
                                let anchor = file_path.parent().unwrap_or(Path::new("."));
                                if let Err(err) =
                                    harn_modules::asset_paths::resolve(&asset_ref, anchor)
                                {
                                    diagnostics.push(PreflightDiagnostic {
                                        code: Code::ImportResolutionFailed,
                                        path: file_path.display().to_string(),
                                        source: source.to_string(),
                                        span: params_arg.map(|arg| arg.span).unwrap_or(node.span),
                                        message: format!("preflight: {err}"),
                                        help: Some(
                                            "see docs/src/modules.md#package-root-prompt-assets for `@/...` and `@<alias>/...` syntax".to_string(),
                                        ),
                                        tags: None,
                                    });
                                    return;
                                }
                            }
                            let resolved =
                                resolve_preflight_target(file_path, &template_path, config);
                            if !resolved
                                .iter()
                                .any(|path| super::result_cache::probe_exists(path))
                            {
                                diagnostics.push(PreflightDiagnostic {
                                    code: Code::PromptTargetMissing,
                                    path: file_path.display().to_string(),
                                    source: source.to_string(),
                                    span: params_arg.map(|arg| arg.span).unwrap_or(node.span),
                                    message: format!(
                                        "preflight: host template render target '{}' does not exist at {}",
                                        template_path,
                                        render_candidate_paths(&resolved)
                                    ),
                                    help: Some(
                                        "verify the template path, or set [check].bundle_root / --bundle-root when validating bundled layouts. Use `@/...` for project-root paths"
                                            .to_string(),
                                    ),
                                    tags: None,
                                });
                            }
                        }
                    }
                }
            } else if let Some(arg) = args.first() {
                diagnostics.push(PreflightDiagnostic {
                    code: Code::CapabilityCallStaticNameRequired,
                    path: file_path.display().to_string(),
                    source: source.to_string(),
                    span: arg.span,
                    message: "preflight: host_call(...) requires a literal \"capability.operation\" name for static validation".to_string(),
                    help: Some(
                        "use a string literal like host_call(\"project.scan\", {}) so preflight can validate the capability contract"
                            .to_string(),
                    ),
                    tags: None,
                });
            }
            scan_children(
                args,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::IfElse {
            condition,
            then_body,
            else_body,
            ..
        } => {
            scan_node_preflight(
                condition,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_children(
                then_body,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            if let Some(else_body) = else_body {
                scan_children(
                    else_body,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
        }
        Node::ForIn { iterable, body, .. }
        | Node::WhileLoop {
            condition: iterable,
            body,
        } => {
            scan_node_preflight(
                iterable,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_children(
                body,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::Retry { count, body } => {
            scan_node_preflight(
                count,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_children(
                body,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::CostRoute { options, body } => {
            for (_, value) in options {
                scan_node_preflight(
                    value,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
            scan_children(
                body,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::ReturnStmt { value } => {
            if let Some(value) = value {
                scan_node_preflight(
                    value,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
        }
        Node::RequireStmt { condition, message } => {
            scan_node_preflight(
                condition,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            if let Some(message) = message {
                scan_node_preflight(
                    message,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
        }
        Node::TryCatch {
            has_catch: _,
            body,
            catch_body,
            finally_body,
            ..
        } => {
            scan_children(
                body,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_children(
                catch_body,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            if let Some(finally_body) = finally_body {
                scan_children(
                    finally_body,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
        }
        Node::TryExpr { body }
        | Node::SpawnExpr { body }
        | Node::ScopeBlock { body }
        | Node::MutexBlock { body, .. }
        | Node::DeferStmt { body } => {
            scan_children(
                body,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::GuardStmt {
            condition,
            else_body,
        } => {
            scan_node_preflight(
                condition,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_children(
                else_body,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::DictLiteral(fields) => {
            for field in fields {
                scan_node_preflight(
                    &field.value,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
        }
        Node::DeadlineBlock { duration, body } => {
            scan_node_preflight(
                duration,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_children(
                body,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::YieldExpr { value } => {
            if let Some(value) = value {
                scan_node_preflight(
                    value,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
        }
        Node::EmitExpr { value } => {
            scan_node_preflight(
                value,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::Parallel { expr, body, .. } => {
            scan_node_preflight(
                expr,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_children(
                body,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::SelectExpr {
            cases,
            timeout,
            default_body,
        } => {
            for case in cases {
                scan_node_preflight(
                    &case.channel,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
                scan_children(
                    &case.body,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
            if let Some((timeout_expr, body)) = timeout {
                scan_node_preflight(
                    timeout_expr,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
                scan_children(
                    body,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
            if let Some(body) = default_body {
                scan_children(
                    body,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
        }
        Node::FunctionCall { args, .. } => {
            scan_children(
                args,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::ValueCall { callee, args } => {
            scan_node_preflight(
                callee,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_children(
                args,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::MethodCall {
            object,
            method,
            args,
        }
        | Node::OptionalMethodCall {
            object,
            method,
            args,
        } => {
            if is_process_execution_method(object, method) {
                scan_execution_dir_preflight(args, file_path, source, diagnostics);
            }
            scan_node_preflight(
                object,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_children(
                args,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::PropertyAccess { object, .. }
        | Node::OptionalPropertyAccess { object, .. }
        | Node::UnaryOp {
            operand: object, ..
        } => {
            scan_node_preflight(
                object,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::SubscriptAccess { object, index }
        | Node::OptionalSubscriptAccess { object, index } => {
            scan_node_preflight(
                object,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_node_preflight(
                index,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::SliceAccess { object, start, end } => {
            scan_node_preflight(
                object,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            if let Some(start) = start {
                scan_node_preflight(
                    start,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
            if let Some(end) = end {
                scan_node_preflight(
                    end,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
        }
        Node::BinaryOp { left, right, .. } => {
            scan_node_preflight(
                left,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_node_preflight(
                right,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => {
            scan_node_preflight(
                condition,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_node_preflight(
                true_expr,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_node_preflight(
                false_expr,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::Assignment { target, value, .. } => {
            scan_node_preflight(
                target,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_node_preflight(
                value,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::ThrowStmt { value } => {
            scan_node_preflight(
                value,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::EnumConstruct { args, .. } | Node::ListLiteral(args) => {
            scan_children(
                args,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::StructConstruct { fields, .. } => {
            for field in fields {
                scan_node_preflight(
                    &field.value,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
        }
        Node::RangeExpr { start, end, .. } => {
            scan_node_preflight(
                start,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            scan_node_preflight(
                end,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::Pipeline { body, .. }
        | Node::OverrideDecl { body, .. }
        | Node::FnDecl { body, .. }
        | Node::ToolDecl { body, .. } => {
            scan_children(
                body,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::SkillDecl { fields, .. } => {
            for (_k, v) in fields {
                scan_node_preflight(
                    v,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
        }
        Node::EvalPackDecl {
            fields,
            body,
            summarize,
            ..
        } => {
            for (_k, v) in fields {
                scan_node_preflight(
                    v,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
            scan_children(
                body,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            if let Some(summary_body) = summarize {
                scan_children(
                    summary_body,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
        }
        Node::LetBinding { value, .. } | Node::ConstBinding { value, .. } => {
            scan_node_preflight(
                value,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::MatchExpr { value, arms } => {
            scan_node_preflight(
                value,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
            for arm in arms {
                scan_children(
                    &arm.body,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
                scan_node_preflight(
                    &arm.pattern,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
        }
        Node::ImplBlock { methods, .. } => {
            scan_children(
                methods,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::Spread(expr)
        | Node::TryOperator { operand: expr }
        | Node::NonNullAssert { operand: expr }
        | Node::TryStar { operand: expr } => {
            scan_node_preflight(
                expr,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::Block(body) | Node::Closure { body, .. } => {
            scan_children(
                body,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::TypeDecl { .. }
        | Node::EnumDecl { .. }
        | Node::StructDecl { .. }
        | Node::InterfaceDecl { .. }
        | Node::DurationLiteral(_)
        | Node::InterpolatedString(_)
        | Node::StringLiteral(_)
        | Node::RawStringLiteral(_)
        | Node::IntLiteral(_)
        | Node::FloatLiteral(_)
        | Node::BoolLiteral(_)
        | Node::NilLiteral
        | Node::Identifier(_)
        | Node::BreakStmt
        | Node::ContinueStmt => {}
        Node::AttributedDecl { inner, .. } => {
            scan_node_preflight(
                inner,
                file_path,
                source,
                config,
                host_capabilities,
                visited,
                diagnostics,
            );
        }
        Node::OrPattern(alternatives) => {
            for alt in alternatives {
                scan_node_preflight(
                    alt,
                    file_path,
                    source,
                    config,
                    host_capabilities,
                    visited,
                    diagnostics,
                );
            }
        }
    }
}

fn scan_children(
    nodes: &[SNode],
    file_path: &Path,
    source: &str,
    config: &CheckConfig,
    host_capabilities: &HostCapabilities,
    visited: &mut HashSet<PathBuf>,
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    for node in nodes {
        scan_node_preflight(
            node,
            file_path,
            source,
            config,
            host_capabilities,
            visited,
            diagnostics,
        );
    }
}
