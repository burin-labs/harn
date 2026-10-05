//! Static tool-surface checks: deferred tools, tool search, and prompt targets.

use super::*;

#[derive(Debug, Clone)]
pub(super) struct StaticToolDef {
    name: String,
    defer_loading: bool,
}

pub(super) fn scan_static_tool_surface_preflight(
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
        let body = if harn_modules::asset_paths::stdlib_prompt_asset_path(&prompt_target).is_some()
        {
            harn_vm::stdlib_modules::get_stdlib_prompt_asset(&prompt_target).map(str::to_string)
        } else {
            let candidates = resolve_preflight_target(file_path, &prompt_target, config);
            let Some(existing) = candidates
                .iter()
                .find(|path| super::super::result_cache::probe_exists(path))
            else {
                continue;
            };
            super::super::result_cache::probe_read_to_string(existing).ok()
        };
        let Some(body) = body else { continue };
        for reference in harn_vm::tool_surface::prompt_tool_references(&body) {
            if !tool_names.contains(&reference) {
                diagnostics.push(PreflightDiagnostic {
                    code: Code::PromptToolSurfaceUnknown,
                    path: file_path.display().to_string(),
                    source: source.to_string(),
                    span: harn_lexer::Span::with_offsets(0, 0, 1, 1),
                    message: format!(
                        "preflight: TOOL_SURFACE_UNKNOWN_PROMPT_TOOL: prompt asset '{prompt_target}' references tool '{reference}' which is not declared in this module's literal tool surface"
                    ),
                    help: Some(
                        "declare the tool with tool_define(...), remove the reference, or mark examples with `harn-tool-surface: ignore-line` / `ignore-next-line`"
                            .to_string(),
                    ),
                    tags: None,
                });
            } else if deferred.contains(&reference) && !tool_search_active {
                diagnostics.push(PreflightDiagnostic {
                    code: Code::PromptToolSurfaceDeferredReference,
                    path: file_path.display().to_string(),
                    source: source.to_string(),
                    span: harn_lexer::Span::with_offsets(0, 0, 1, 1),
                    message: format!(
                        "preflight: TOOL_SURFACE_DEFERRED_TOOL_PROMPT_REFERENCE: prompt asset '{prompt_target}' references deferred tool '{reference}' but no literal tool_search option is active"
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

pub(super) fn collect_static_tool_surface_from_node(
    node: &SNode,
    tool_defs: &mut Vec<StaticToolDef>,
    prompt_targets: &mut Vec<String>,
    tool_search_active: &mut bool,
) {
    match &node.node {
        Node::FunctionCall { name, args, .. } if name == "tool_define" => {
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
        Node::MethodCall { method, args, .. }
            if matches!(
                method.as_str(),
                "render_prompt" | "render_prompt_with_provenance"
            ) =>
        {
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
        Node::FunctionCall { name, args, .. } if name == "agent_loop" || name == "llm_call" => {
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
        | Node::ScopeBlock { body }
        | Node::DeferStmt { body }
        | Node::MutexBlock { body, .. }
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
        | Node::ConstBinding { value, .. }
        | Node::Assignment { value, .. }
        | Node::ThrowStmt { value }
        | Node::EmitExpr { value }
        | Node::YieldExpr { value: Some(value) }
        | Node::ReturnStmt { value: Some(value) }
        | Node::Spread(value)
        | Node::TryOperator { operand: value }
        | Node::NonNullAssert { operand: value }
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
            ..
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
        Node::EvalPackDecl {
            fields,
            body,
            summarize,
            ..
        } => {
            for (_, value) in fields {
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
            if let Some(summary_body) = summarize {
                for child in summary_body {
                    collect_static_tool_surface_from_node(
                        child,
                        tool_defs,
                        prompt_targets,
                        tool_search_active,
                    );
                }
            }
        }
        Node::TryCatch {
            has_catch: _,
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
