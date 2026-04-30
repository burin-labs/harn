use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use harn_modules::resolve_import_path;
use harn_parser::{Node, SNode};

use crate::package::CheckConfig;
use crate::parse_source_file;

use super::host_capabilities::{is_known_host_operation, load_host_capabilities};
use super::imports::{scan_import_collisions, scan_re_export_conflicts};
use super::mock_host::collect_mock_host_capabilities;

pub(super) struct PreflightDiagnostic {
    pub(super) path: String,
    pub(super) source: String,
    pub(super) span: harn_lexer::Span,
    pub(super) message: String,
    pub(super) help: Option<String>,
    /// Optional `"capability.operation"` tag used for per-capability
    /// suppression via `[check].preflight_allow`. `None` means this
    /// diagnostic is not scoped to a single host capability.
    pub(super) tags: Option<String>,
}

/// Returns `true` when `tag` matches any entry in `allow`. Entries may
/// be exact (`project.scan`), capability wildcards (`project.*` or
/// just `project`), or a literal `*` to suppress every preflight
/// diagnostic that carries a tag.
pub(super) fn is_preflight_allowed(tag: &Option<String>, allow: &[String]) -> bool {
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

pub(super) fn collect_preflight_diagnostics(
    path: &Path,
    source: &str,
    program: &[SNode],
    config: &CheckConfig,
) -> Vec<PreflightDiagnostic> {
    let mut diagnostics = Vec::new();
    let mut visited = HashSet::new();
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut host_capabilities = load_host_capabilities(config);
    let mut mocked_caps_visited = HashSet::new();
    collect_mock_host_capabilities(
        &canonical,
        source,
        program,
        &mut mocked_caps_visited,
        &mut host_capabilities,
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

    scan_import_collisions(&canonical, source, program, &mut diagnostics);
    scan_re_export_conflicts(&canonical, source, program, &mut diagnostics);
    scan_static_tool_surface_preflight(&canonical, source, program, config, &mut diagnostics);

    diagnostics
}

#[derive(Debug, Clone)]
struct StaticToolDef {
    name: String,
    defer_loading: bool,
}

fn scan_static_tool_surface_preflight(
    file_path: &Path,
    source: &str,
    program: &[SNode],
    config: &CheckConfig,
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    let mut tool_defs = Vec::new();
    let mut prompt_targets = Vec::new();
    let mut tool_search_active = false;
    for node in program {
        collect_static_tool_surface_from_node(
            node,
            &mut tool_defs,
            &mut prompt_targets,
            &mut tool_search_active,
        );
    }
    if tool_defs.is_empty() {
        return;
    }
    let tool_names = tool_defs
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let deferred = tool_defs
        .iter()
        .filter(|tool| tool.defer_loading)
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    for prompt_target in prompt_targets {
        let candidates = resolve_preflight_target(file_path, &prompt_target, config);
        let Some(existing) = candidates.iter().find(|path| path.exists()) else {
            continue;
        };
        let Ok(body) = std::fs::read_to_string(existing) else {
            continue;
        };
        for reference in harn_vm::tool_surface::prompt_tool_references(&body) {
            if !tool_names.contains(&reference) {
                diagnostics.push(PreflightDiagnostic {
                    path: file_path.display().to_string(),
                    source: source.to_string(),
                    span: harn_lexer::Span::with_offsets(0, 0, 1, 1),
                    message: format!(
                        "preflight: TOOL_SURFACE_UNKNOWN_PROMPT_TOOL: prompt asset '{}' references tool '{}' which is not declared in this module's literal tool surface",
                        prompt_target, reference
                    ),
                    help: Some(
                        "declare the tool with tool_define(...), remove the reference, or mark examples with `harn-tool-surface: ignore-line` / `ignore-next-line`"
                            .to_string(),
                    ),
                    tags: None,
                });
            } else if deferred.contains(&reference) && !tool_search_active {
                diagnostics.push(PreflightDiagnostic {
                    path: file_path.display().to_string(),
                    source: source.to_string(),
                    span: harn_lexer::Span::with_offsets(0, 0, 1, 1),
                    message: format!(
                        "preflight: TOOL_SURFACE_DEFERRED_TOOL_PROMPT_REFERENCE: prompt asset '{}' references deferred tool '{}' but no literal tool_search option is active",
                        prompt_target, reference
                    ),
                    help: Some(
                        "enable tool_search for the agent loop, make the tool eager, or mark historical/example text as ignored"
                            .to_string(),
                    ),
                    tags: None,
                });
            }
        }
    }
}

fn collect_static_tool_surface_from_node(
    node: &SNode,
    tool_defs: &mut Vec<StaticToolDef>,
    prompt_targets: &mut Vec<String>,
    tool_search_active: &mut bool,
) {
    match &node.node {
        Node::FunctionCall { name, args } if name == "tool_define" => {
            if let Some(tool_name) = args.get(1).and_then(literal_string) {
                let defer_loading = args
                    .get(3)
                    .and_then(|config| dict_literal_field(config, "defer_loading"))
                    .is_some_and(|value| matches!(value.node, Node::BoolLiteral(true)));
                tool_defs.push(StaticToolDef {
                    name: tool_name,
                    defer_loading,
                });
            }
            for arg in args {
                collect_static_tool_surface_from_node(
                    arg,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
            }
        }
        Node::FunctionCall { name, args } if name == "render_prompt" => {
            if let Some(target) = args.first().and_then(literal_template_path) {
                prompt_targets.push(target);
            }
            for arg in args {
                collect_static_tool_surface_from_node(
                    arg,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
            }
        }
        Node::FunctionCall { name, args } if name == "agent_loop" || name == "llm_call" => {
            if args
                .get(2)
                .and_then(|options| dict_literal_field(options, "tool_search"))
                .is_some_and(|value| {
                    !matches!(value.node, Node::BoolLiteral(false) | Node::NilLiteral)
                })
            {
                *tool_search_active = true;
            }
            for arg in args {
                collect_static_tool_surface_from_node(
                    arg,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
            }
        }
        Node::Pipeline { body, .. }
        | Node::ImplBlock { methods: body, .. }
        | Node::OverrideDecl { body, .. }
        | Node::FnDecl { body, .. }
        | Node::ToolDecl { body, .. }
        | Node::SpawnExpr { body }
        | Node::DeferStmt { body }
        | Node::MutexBlock { body }
        | Node::Block(body)
        | Node::Closure { body, .. }
        | Node::TryExpr { body } => {
            for child in body {
                collect_static_tool_surface_from_node(
                    child,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
            }
        }
        Node::LetBinding { value, .. }
        | Node::VarBinding { value, .. }
        | Node::Assignment { value, .. }
        | Node::ThrowStmt { value }
        | Node::EmitExpr { value }
        | Node::YieldExpr { value: Some(value) }
        | Node::ReturnStmt { value: Some(value) }
        | Node::Spread(value)
        | Node::TryOperator { operand: value }
        | Node::TryStar { operand: value }
        | Node::UnaryOp { operand: value, .. } => collect_static_tool_surface_from_node(
            value,
            tool_defs,
            prompt_targets,
            tool_search_active,
        ),
        Node::IfElse {
            condition,
            then_body,
            else_body,
        } => {
            collect_static_tool_surface_from_node(
                condition,
                tool_defs,
                prompt_targets,
                tool_search_active,
            );
            for child in then_body.iter().chain(else_body.iter().flatten()) {
                collect_static_tool_surface_from_node(
                    child,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
            }
        }
        Node::ForIn { iterable, body, .. }
        | Node::WhileLoop {
            condition: iterable,
            body,
        } => {
            collect_static_tool_surface_from_node(
                iterable,
                tool_defs,
                prompt_targets,
                tool_search_active,
            );
            for child in body {
                collect_static_tool_surface_from_node(
                    child,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
            }
        }
        Node::Retry { count, body }
        | Node::DeadlineBlock {
            duration: count,
            body,
        } => {
            collect_static_tool_surface_from_node(
                count,
                tool_defs,
                prompt_targets,
                tool_search_active,
            );
            for child in body {
                collect_static_tool_surface_from_node(
                    child,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
            }
        }
        Node::CostRoute { options, body } => {
            for (_, value) in options {
                collect_static_tool_surface_from_node(
                    value,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
            }
            for child in body {
                collect_static_tool_surface_from_node(
                    child,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
            }
        }
        Node::SkillDecl { fields, .. } => {
            for (_, value) in fields {
                collect_static_tool_surface_from_node(
                    value,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
            }
        }
        Node::TryCatch {
            body,
            catch_body,
            finally_body,
            ..
        } => {
            for child in body
                .iter()
                .chain(catch_body)
                .chain(finally_body.iter().flatten())
            {
                collect_static_tool_surface_from_node(
                    child,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
            }
        }
        Node::GuardStmt {
            condition,
            else_body,
        } => {
            collect_static_tool_surface_from_node(
                condition,
                tool_defs,
                prompt_targets,
                tool_search_active,
            );
            for child in else_body {
                collect_static_tool_surface_from_node(
                    child,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
            }
        }
        Node::RequireStmt { condition, message } => {
            collect_static_tool_surface_from_node(
                condition,
                tool_defs,
                prompt_targets,
                tool_search_active,
            );
            if let Some(message) = message {
                collect_static_tool_surface_from_node(
                    message,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
            }
        }
        Node::MatchExpr { value, arms } => {
            collect_static_tool_surface_from_node(
                value,
                tool_defs,
                prompt_targets,
                tool_search_active,
            );
            for arm in arms {
                collect_static_tool_surface_from_node(
                    &arm.pattern,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
                if let Some(guard) = &arm.guard {
                    collect_static_tool_surface_from_node(
                        guard,
                        tool_defs,
                        prompt_targets,
                        tool_search_active,
                    );
                }
                for child in &arm.body {
                    collect_static_tool_surface_from_node(
                        child,
                        tool_defs,
                        prompt_targets,
                        tool_search_active,
                    );
                }
            }
        }
        Node::FunctionCall { args, .. } | Node::EnumConstruct { args, .. } => {
            for arg in args {
                collect_static_tool_surface_from_node(
                    arg,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
            }
        }
        Node::MethodCall { object, args, .. } | Node::OptionalMethodCall { object, args, .. } => {
            collect_static_tool_surface_from_node(
                object,
                tool_defs,
                prompt_targets,
                tool_search_active,
            );
            for arg in args {
                collect_static_tool_surface_from_node(
                    arg,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
            }
        }
        Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
            collect_static_tool_surface_from_node(
                object,
                tool_defs,
                prompt_targets,
                tool_search_active,
            );
        }
        Node::SubscriptAccess { object, index }
        | Node::OptionalSubscriptAccess { object, index } => {
            collect_static_tool_surface_from_node(
                object,
                tool_defs,
                prompt_targets,
                tool_search_active,
            );
            collect_static_tool_surface_from_node(
                index,
                tool_defs,
                prompt_targets,
                tool_search_active,
            );
        }
        Node::SliceAccess { object, start, end } => {
            collect_static_tool_surface_from_node(
                object,
                tool_defs,
                prompt_targets,
                tool_search_active,
            );
            if let Some(start) = start {
                collect_static_tool_surface_from_node(
                    start,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
            }
            if let Some(end) = end {
                collect_static_tool_surface_from_node(
                    end,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
            }
        }
        Node::BinaryOp { left, right, .. }
        | Node::RangeExpr {
            start: left,
            end: right,
            ..
        } => {
            collect_static_tool_surface_from_node(
                left,
                tool_defs,
                prompt_targets,
                tool_search_active,
            );
            collect_static_tool_surface_from_node(
                right,
                tool_defs,
                prompt_targets,
                tool_search_active,
            );
        }
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => {
            collect_static_tool_surface_from_node(
                condition,
                tool_defs,
                prompt_targets,
                tool_search_active,
            );
            collect_static_tool_surface_from_node(
                true_expr,
                tool_defs,
                prompt_targets,
                tool_search_active,
            );
            collect_static_tool_surface_from_node(
                false_expr,
                tool_defs,
                prompt_targets,
                tool_search_active,
            );
        }
        Node::Parallel {
            expr,
            body,
            options,
            ..
        } => {
            collect_static_tool_surface_from_node(
                expr,
                tool_defs,
                prompt_targets,
                tool_search_active,
            );
            for (_, option) in options {
                collect_static_tool_surface_from_node(
                    option,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
            }
            for child in body {
                collect_static_tool_surface_from_node(
                    child,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
            }
        }
        Node::SelectExpr {
            cases,
            timeout,
            default_body,
        } => {
            for case in cases {
                collect_static_tool_surface_from_node(
                    &case.channel,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
                for child in &case.body {
                    collect_static_tool_surface_from_node(
                        child,
                        tool_defs,
                        prompt_targets,
                        tool_search_active,
                    );
                }
            }
            if let Some((duration, body)) = timeout {
                collect_static_tool_surface_from_node(
                    duration,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
                for child in body {
                    collect_static_tool_surface_from_node(
                        child,
                        tool_defs,
                        prompt_targets,
                        tool_search_active,
                    );
                }
            }
            for child in default_body.iter().flatten() {
                collect_static_tool_surface_from_node(
                    child,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
            }
        }
        Node::ListLiteral(items) | Node::OrPattern(items) => {
            for item in items {
                collect_static_tool_surface_from_node(
                    item,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
            }
        }
        Node::DictLiteral(entries)
        | Node::StructConstruct {
            fields: entries, ..
        } => {
            for entry in entries {
                collect_static_tool_surface_from_node(
                    &entry.key,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
                collect_static_tool_surface_from_node(
                    &entry.value,
                    tool_defs,
                    prompt_targets,
                    tool_search_active,
                );
            }
        }
        Node::AttributedDecl { inner, .. } => collect_static_tool_surface_from_node(
            inner,
            tool_defs,
            prompt_targets,
            tool_search_active,
        ),
        _ => {}
    }
}

fn scan_program_preflight(
    file_path: &Path,
    source: &str,
    program: &[SNode],
    config: &CheckConfig,
    host_capabilities: &HashMap<String, HashSet<String>>,
    visited: &mut HashSet<PathBuf>,
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    let canonical = file_path
        .canonicalize()
        .unwrap_or_else(|_| file_path.to_path_buf());
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
    host_capabilities: &HashMap<String, HashSet<String>>,
    visited: &mut HashSet<PathBuf>,
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    match &node.node {
        Node::ImportDecl { path, .. } | Node::SelectiveImport { path, .. } => {
            if path.starts_with("std/") {
                return;
            }
            match resolve_import_path(file_path, path) {
                Some(import_path) => {
                    let import_str = import_path.to_string_lossy().into_owned();
                    let (import_source, import_program) = parse_source_file(&import_str);
                    scan_program_preflight(
                        &import_path,
                        &import_source,
                        &import_program,
                        config,
                        host_capabilities,
                        visited,
                        diagnostics,
                    );
                }
                None => diagnostics.push(PreflightDiagnostic {
                    path: file_path.display().to_string(),
                    source: source.to_string(),
                    span: node.span,
                    message: format!("preflight: unresolved import '{path}'"),
                    help: Some("verify the import path and packaged module layout".to_string()),
                    tags: None,
                }),
            }
        }
        Node::FunctionCall { name, args } if name == "render" || name == "render_prompt" => {
            if let Some(template_path) = args.first().and_then(literal_template_path) {
                if let Some(asset_ref) = harn_modules::asset_paths::parse(&template_path) {
                    let anchor = file_path.parent().unwrap_or(Path::new("."));
                    if let Err(err) = harn_modules::asset_paths::resolve(&asset_ref, anchor) {
                        // Surface resolver errors (no project root, unknown
                        // alias) before the file-existence check so the
                        // user sees the structural cause, not a generic
                        // "render target does not exist" message.
                        diagnostics.push(PreflightDiagnostic {
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
                if let Some(existing) = resolved.iter().find(|path| path.exists()) {
                    if let Ok(body) = std::fs::read_to_string(existing) {
                        if let Err(err) = harn_vm::stdlib::template::validate_template_syntax(&body)
                        {
                            diagnostics.push(PreflightDiagnostic {
                                path: file_path.display().to_string(),
                                source: source.to_string(),
                                span: args[0].span,
                                message: format!(
                                    "preflight: template '{}' has a syntax error: {err}",
                                    template_path
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
                        path: file_path.display().to_string(),
                        source: source.to_string(),
                        span: args[0].span,
                        message: format!(
                            "preflight: {name} target '{}' does not exist at {}",
                            template_path,
                            render_candidate_paths(&resolved)
                        ),
                        help: Some(render_target_miss_help(file_path, &template_path)),
                        tags: None,
                    });
                }
            }
        }
        Node::FunctionCall { name, args } if name == "exec_at" || name == "shell_at" => {
            if let Some(dir) = args.first().and_then(literal_string) {
                let resolved = resolve_source_relative(file_path, &dir);
                if !resolved.is_dir() {
                    diagnostics.push(PreflightDiagnostic {
                        path: file_path.display().to_string(),
                        source: source.to_string(),
                        span: args[0].span,
                        message: format!(
                            "preflight: execution directory '{}' does not exist at {}",
                            dir,
                            resolved.display()
                        ),
                        help: Some(
                            "use a source-relative directory that exists at preflight time, or create it before execution"
                                .to_string(),
                        ),
                        tags: None,
                    });
                }
            }
        }
        Node::FunctionCall { name, args } if name == "spawn_agent" => {
            if let Some(agent_config) = args.first() {
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
        Node::FunctionCall { name, args } if name == "host_invoke" => {
            diagnostics.push(PreflightDiagnostic {
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
        Node::FunctionCall { name, args } if name == "tool_define" => {
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
        Node::FunctionCall { name, args } if name == "host_call" => {
            if let Some((cap, op, params_arg)) = parse_host_call_args(args) {
                if !is_known_host_operation(host_capabilities, &cap, &op) {
                    diagnostics.push(PreflightDiagnostic {
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
                }
                if cap == "template" && op == "render" {
                    if let Some(template_path) = host_render_path_arg(params_arg) {
                        if let Some(asset_ref) = harn_modules::asset_paths::parse(&template_path) {
                            let anchor = file_path.parent().unwrap_or(Path::new("."));
                            if let Err(err) = harn_modules::asset_paths::resolve(&asset_ref, anchor)
                            {
                                diagnostics.push(PreflightDiagnostic {
                                    path: file_path.display().to_string(),
                                    source: source.to_string(),
                                    span: params_arg
                                        .map(|arg| arg.span)
                                        .unwrap_or(node.span),
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
                        if !resolved.iter().any(|path| path.exists()) {
                            diagnostics.push(PreflightDiagnostic {
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
            } else if let Some(arg) = args.first() {
                diagnostics.push(PreflightDiagnostic {
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
        | Node::MutexBlock { body }
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
        Node::MethodCall { object, args, .. } | Node::OptionalMethodCall { object, args, .. } => {
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
        Node::LetBinding { value, .. } | Node::VarBinding { value, .. } => {
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
    host_capabilities: &HashMap<String, HashSet<String>>,
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

pub(super) fn resolve_source_relative(current_file: &Path, target: &str) -> PathBuf {
    let candidate = PathBuf::from(target);
    if candidate.is_absolute() {
        candidate
    } else {
        current_file
            .parent()
            .unwrap_or(Path::new("."))
            .join(candidate)
    }
}

pub(super) fn resolve_preflight_target(
    current_file: &Path,
    target: &str,
    config: &CheckConfig,
) -> Vec<PathBuf> {
    // `@/...` and `@<alias>/...` always anchor at the project root of
    // the calling file, never at the bundle root or the source dir, so
    // the preflight scan must use the same resolver as the runtime
    // (issue #742). When resolution fails (no harn.toml ancestor /
    // unknown alias) we still surface a single candidate path so the
    // caller's diagnostic explains why the target was unreachable.
    if let Some(asset_ref) = harn_modules::asset_paths::parse(target) {
        let anchor = current_file.parent().unwrap_or(Path::new("."));
        return match harn_modules::asset_paths::resolve(&asset_ref, anchor) {
            Ok(path) => vec![path],
            Err(_) => vec![PathBuf::from(target)],
        };
    }
    let mut candidates = vec![resolve_source_relative(current_file, target)];
    if let Some(bundle_root) = config.bundle_root.as_deref() {
        let bundle_base = PathBuf::from(bundle_root);
        candidates.push(if PathBuf::from(target).is_absolute() {
            PathBuf::from(target)
        } else {
            bundle_base.join(target)
        });
    }
    candidates.dedup();
    candidates
}

fn render_candidate_paths(candidates: &[PathBuf]) -> String {
    candidates
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(" or ")
}

pub(super) fn host_render_path_arg(arg: Option<&SNode>) -> Option<String> {
    let Node::DictLiteral(entries) = &arg?.node else {
        return None;
    };
    entries
        .iter()
        .find_map(|entry| match (&entry.key.node, &entry.value.node) {
            (Node::Identifier(key), Node::StringLiteral(path)) if key == "path" => {
                Some(path.clone())
            }
            (Node::StringLiteral(key), Node::StringLiteral(path)) if key == "path" => {
                Some(path.clone())
            }
            _ => None,
        })
}

pub(super) fn parse_host_call_args(args: &[SNode]) -> Option<(String, String, Option<&SNode>)> {
    let Node::StringLiteral(name) = &args.first()?.node else {
        return None;
    };
    let (capability, operation) = name.split_once('.')?;
    Some((capability.to_string(), operation.to_string(), args.get(1)))
}

pub(super) fn literal_string(node: &SNode) -> Option<String> {
    match &node.node {
        Node::StringLiteral(value) => Some(value.clone()),
        _ => None,
    }
}

/// `render(...)` and `render_prompt(...)` accept either form of static
/// string literal as their first argument. Both are statically
/// verifiable; only `InterpolatedString` and arbitrary expressions are
/// dynamic and must be skipped.
fn literal_template_path(node: &SNode) -> Option<String> {
    match &node.node {
        Node::StringLiteral(value) | Node::RawStringLiteral(value) => Some(value.clone()),
        _ => None,
    }
}

/// Build the help text for a missing `render(...)` / `render_prompt(...)`
/// target. When the basename can be located somewhere else under the
/// caller's project root (and the search produces a unique hit), prepend
/// a "did you mean ...?" suggestion so the most common typo — file
/// misfiled in a sibling directory — is one keystroke from a fix. Falls
/// back to the generic guidance when the search is ambiguous or finds
/// nothing.
fn render_target_miss_help(file_path: &Path, template_path: &str) -> String {
    const GENERIC: &str = "keep template paths relative to the pipeline source file, or set [check].bundle_root / --bundle-root for bundled layouts. Use `@/...` for project-root paths";
    let Some(basename) = Path::new(template_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
    else {
        return GENERIC.to_string();
    };
    let anchor = file_path.parent().unwrap_or(Path::new("."));
    let project_root = harn_modules::asset_paths::find_project_root(anchor)
        .unwrap_or_else(|| anchor.to_path_buf());
    let Some(near) = find_unique_basename(&project_root, &basename) else {
        return GENERIC.to_string();
    };
    let caller_dir = file_path.parent();
    if near.parent() == caller_dir {
        // The runtime would have found the file at the caller's dir
        // anyway; avoid suggesting a redundant "did you mean ...?".
        return GENERIC.to_string();
    }
    let display = near
        .strip_prefix(&project_root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| near.display().to_string());
    format!(
        "did you mean '{display}'? (found at {}). Otherwise: {GENERIC}",
        near.display()
    )
}

/// Returns the unique location of `basename` under `root`, or `None`
/// when the search finds zero or multiple matches. Skips standard
/// build/dependency directories so a misfiled prompt is not lost in
/// vendor noise.
fn find_unique_basename(root: &Path, basename: &str) -> Option<PathBuf> {
    let mut matches: Vec<PathBuf> = Vec::with_capacity(2);
    walk_for_basename(root, basename, 0, 8, &mut matches);
    (matches.len() == 1).then(|| matches.into_iter().next().expect("len == 1"))
}

fn walk_for_basename(
    dir: &Path,
    basename: &str,
    depth: usize,
    max_depth: usize,
    out: &mut Vec<PathBuf>,
) {
    if depth > max_depth || out.len() > 1 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();
        if name_str.starts_with('.')
            || matches!(
                name_str.as_ref(),
                "target" | "node_modules" | "dist" | "build" | "out" | ".harn-runs"
            )
        {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_file() {
            if name_str == basename {
                out.push(path);
                if out.len() > 1 {
                    return;
                }
            }
        } else if file_type.is_dir() {
            walk_for_basename(&path, basename, depth + 1, max_depth, out);
            if out.len() > 1 {
                return;
            }
        }
    }
}

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
fn scan_tool_define_preflight(
    node: &SNode,
    args: &[SNode],
    host_capabilities: &HashMap<String, HashSet<String>>,
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

pub(super) fn dict_literal_field<'a>(node: &'a SNode, field: &str) -> Option<&'a SNode> {
    let Node::DictLiteral(entries) = &node.node else {
        return None;
    };
    entries.iter().find_map(|entry| match &entry.key.node {
        Node::Identifier(key) | Node::StringLiteral(key) if key == field => Some(&entry.value),
        _ => None,
    })
}

fn scan_spawn_agent_preflight(
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
        if !resolved.is_dir() {
            diagnostics.push(PreflightDiagnostic {
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
        if !resolved.is_dir() {
            diagnostics.push(PreflightDiagnostic {
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
