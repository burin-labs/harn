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
    harn_parser::visit::walk_node(node, &mut |node| match &node.node {
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
        }
        Node::MethodCall { method, args, .. } if matches!(method.as_str(), "render_prompt") => {
            if let Some(target) = args.first().and_then(literal_template_path) {
                prompt_targets.push(target);
            }
        }
        Node::FunctionCall { name, args, .. }
            if (name == "agent_loop" || name == "llm_call")
                && args
                    .get(2)
                    .and_then(|options| dict_literal_field(options, "tool_search"))
                    .is_some_and(|value| {
                        !matches!(value.node, Node::BoolLiteral(false) | Node::NilLiteral)
                    }) =>
        {
            *tool_search_active = true;
        }
        _ => {}
    });
}
