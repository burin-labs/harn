//! Static `spawn_agent` effect-inheritance checks.

use super::*;

/// E5.4 (`HARN-CAP-301`) — verify that any `spawn_agent({...})` call has
/// a child effect set that is a subset of the enclosing parent's. Both
/// effect sets are derived via the same `compute_handoff_effects`
/// analyzer the runtime guard uses, so the static path and the runtime
/// path stay in agreement.
///
/// The scan is conservative: it only runs when both the parent fn/pipeline
/// body source and the spawn_agent argument source can be sliced out of
/// the program. Programs that build the spawn config dynamically fall
/// through to runtime enforcement.
pub(super) fn scan_effect_inheritance_preflight(
    file_path: &Path,
    source: &str,
    program: &[SNode],
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    for node in program {
        scan_effect_inheritance_in_decl(node, file_path, source, diagnostics);
    }
}

pub(super) fn scan_effect_inheritance_in_decl(
    node: &SNode,
    file_path: &Path,
    source: &str,
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    let (parent_body, parent_label) = match &node.node {
        Node::FnDecl { name, body, .. } => (body.as_slice(), name.clone()),
        Node::Pipeline { name, body, .. } => (body.as_slice(), name.clone()),
        Node::AttributedDecl { inner, .. } => {
            return scan_effect_inheritance_in_decl(inner, file_path, source, diagnostics);
        }
        _ => return,
    };
    let parent_source = source_excluding_spawn_agents(source, parent_body);
    let parent_effects = if parent_source.trim().is_empty() {
        Vec::new()
    } else {
        // Wrap so `compute_handoff_effects`'s parser sees a complete
        // top-level item; the wrapping fn just hosts the body's calls.
        let wrapped = format!("fn __parent_probe(harness: Harness) {{\n{parent_source}\n}}");
        harn_vm::orchestration::compute_handoff_effects(&wrapped, None)
    };
    let mut spawn_sites: Vec<(harn_lexer::Span, String)> = Vec::new();
    for body_node in parent_body {
        collect_spawn_agent_sites(body_node, source, &mut spawn_sites);
    }
    for (span, config_source) in spawn_sites {
        let wrapped_child = format!("fn __child_probe(harness: Harness) {{\n{config_source}\n}}");
        let child_effects = harn_vm::orchestration::compute_handoff_effects(&wrapped_child, None);
        if child_effects.is_empty() {
            continue;
        }
        let violations =
            harn_vm::orchestration::effect_subset_violations(Some(&parent_effects), &child_effects);
        if violations.is_empty() {
            continue;
        }
        let summary: Vec<String> = violations
            .iter()
            .map(harn_vm::orchestration::effect_record_summary)
            .collect();
        diagnostics.push(PreflightDiagnostic {
            code: Code::EffectInheritanceViolation,
            path: file_path.display().to_string(),
            source: source.to_string(),
            span,
            message: format!(
                "preflight: spawn_agent in '{parent_label}' grants effects the parent does not declare: {effects}",
                effects = summary.join(", ")
            ),
            help: Some(
                "narrow the child agent's effects to a subset of the parent's, or widen the parent's declared effects (repair: policy/narrow-child-effects, safety: surface-changing)"
                    .to_string(),
            ),
            tags: None,
        });
    }
}

#[expect(
    clippy::string_slice,
    reason = "lexer spans over this source are char-boundary offsets; source.len() clamp is too"
)]
pub(super) fn source_excluding_spawn_agents(source: &str, body: &[SNode]) -> String {
    let mut spawn_spans: Vec<harn_lexer::Span> = Vec::new();
    for node in body {
        collect_spawn_agent_spans(node, &mut spawn_spans);
    }
    spawn_spans.sort_by_key(|span| span.start);

    let Some(first) = body.first() else {
        return String::new();
    };
    let Some(last) = body.last() else {
        return String::new();
    };
    let body_start = first.span.start.min(source.len());
    let body_end = last.span.end.min(source.len());
    if body_end <= body_start {
        return String::new();
    }
    let mut out = String::with_capacity(body_end - body_start);
    let mut cursor = body_start;
    for span in spawn_spans {
        let s = span.start.max(body_start).min(body_end);
        let e = span.end.max(body_start).min(body_end);
        if s >= cursor {
            out.push_str(&source[cursor..s]);
            // Replace the elided spawn_agent call with a `nil` so the
            // surrounding statement structure (e.g. `let x = ...;`)
            // still parses.
            out.push_str("nil");
            cursor = e;
        }
    }
    if cursor < body_end {
        out.push_str(&source[cursor..body_end]);
    }
    out
}

pub(super) fn collect_spawn_agent_spans(node: &SNode, out: &mut Vec<harn_lexer::Span>) {
    if let Node::FunctionCall { name, .. } = &node.node {
        if name == "spawn_agent" {
            out.push(node.span);
            return;
        }
    }
    for child in spawn_site_children(node) {
        collect_spawn_agent_spans(child, out);
    }
}

#[expect(
    clippy::string_slice,
    reason = "lexer spans over this source are char-boundary offsets; source.len() clamp is too"
)]
pub(super) fn collect_spawn_agent_sites(
    node: &SNode,
    source: &str,
    out: &mut Vec<(harn_lexer::Span, String)>,
) {
    if let Node::FunctionCall { name, args, .. } = &node.node {
        if name == "spawn_agent" {
            if let Some(config) = args.last() {
                let start = config.span.start.min(source.len());
                let end = config.span.end.min(source.len());
                if end > start {
                    out.push((node.span, source[start..end].to_string()));
                }
            }
            return;
        }
    }
    for child in spawn_site_children(node) {
        collect_spawn_agent_sites(child, source, out);
    }
}

pub(super) fn spawn_site_children(node: &SNode) -> Vec<&SNode> {
    harn_parser::visit::immediate_children(node)
}
