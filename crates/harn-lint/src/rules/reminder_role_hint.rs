//! `reminder-role-hint-capability` rule: warn when a pipeline hardcodes
//! `role_hint: "user_block"` while also hardcoding an LLM provider/model
//! route that cannot preserve that shape.

use harn_lexer::Span;
use harn_parser::visit;
use harn_parser::{DiagnosticCode as Code, DictEntry, Node, SNode};
use harn_vm::llm::capabilities;

use crate::diagnostic::{LintDiagnostic, LintSeverity};

const RULE_NAME: &str = "reminder-role-hint-capability";

const TARGET_CALLEES: &[&str] = &[
    "llm_call",
    "llm_call_safe",
    "llm_call_structured",
    "llm_call_structured_result",
    "agent_loop",
];

#[derive(Debug, Clone)]
struct LlmRoute {
    provider: String,
    model: String,
}

/// Walk each pipeline and warn when a literal reminder role hint is
/// paired with a literal provider route that will degrade it to system
/// text. Dynamic providers are skipped because the capability surface is
/// not knowable statically.
pub(crate) fn check_reminder_role_hint_capabilities(
    program: &[SNode],
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    for node in program {
        let body = match &node.node {
            Node::Pipeline { body, .. } => body.as_slice(),
            Node::AttributedDecl { inner, .. } => match &inner.node {
                Node::Pipeline { body, .. } => body.as_slice(),
                _ => continue,
            },
            _ => continue,
        };
        check_pipeline_body(body, diagnostics);
    }
}

fn check_pipeline_body(body: &[SNode], diagnostics: &mut Vec<LintDiagnostic>) {
    let mut user_block_spans = Vec::new();
    let mut routes = Vec::new();

    visit::walk_program(body, &mut |node| match &node.node {
        Node::DictLiteral(entries)
        | Node::StructConstruct {
            fields: entries, ..
        } => {
            collect_user_block_role_hints(entries, &mut user_block_spans);
        }
        Node::FunctionCall { name, args, .. } if TARGET_CALLEES.contains(&name.as_str()) => {
            for arg in args {
                if let Node::DictLiteral(entries) = &arg.node {
                    if let Some(route) = literal_route_from_entries(entries) {
                        routes.push(route);
                    }
                }
            }
        }
        Node::CostRoute { options, .. } => {
            if let Some(route) = literal_route_from_options(options) {
                routes.push(route);
            }
        }
        _ => {}
    });

    let unsupported = routes
        .iter()
        .find(|route| !route_supports_user_block(route));
    let Some(route) = unsupported else {
        return;
    };

    for span in user_block_spans {
        diagnostics.push(make_diagnostic(route, span));
    }
}

fn collect_user_block_role_hints(entries: &[DictEntry], spans: &mut Vec<Span>) {
    for entry in entries {
        if key_name(&entry.key).as_deref() == Some("role_hint")
            && literal_string(&entry.value).as_deref() == Some("user_block")
        {
            spans.push(entry.value.span);
        }
    }
}

fn literal_route_from_entries(entries: &[DictEntry]) -> Option<LlmRoute> {
    let provider = literal_value_for_key(entries, "provider")?;
    if provider == "auto" {
        return None;
    }
    Some(LlmRoute {
        provider,
        model: literal_value_for_key(entries, "model").unwrap_or_default(),
    })
}

fn literal_route_from_options(options: &[(String, SNode)]) -> Option<LlmRoute> {
    let provider = options
        .iter()
        .find(|(key, _)| key == "provider")
        .and_then(|(_, value)| literal_string(value))?;
    if provider == "auto" {
        return None;
    }
    Some(LlmRoute {
        provider,
        model: options
            .iter()
            .find(|(key, _)| key == "model")
            .and_then(|(_, value)| literal_string(value))
            .unwrap_or_default(),
    })
}

fn literal_value_for_key(entries: &[DictEntry], key: &str) -> Option<String> {
    entries
        .iter()
        .find(|entry| key_name(&entry.key).as_deref() == Some(key))
        .and_then(|entry| literal_string(&entry.value))
}

fn key_name(node: &SNode) -> Option<String> {
    match &node.node {
        Node::StringLiteral(value) | Node::RawStringLiteral(value) | Node::Identifier(value) => {
            Some(value.clone())
        }
        _ => None,
    }
}

fn literal_string(node: &SNode) -> Option<String> {
    match &node.node {
        Node::StringLiteral(value) | Node::RawStringLiteral(value) => Some(value.clone()),
        _ => None,
    }
}

fn route_supports_user_block(route: &LlmRoute) -> bool {
    let caps = capabilities::lookup(&route.provider, &route.model);
    caps.message_wire_format == "anthropic" || caps.prefers_role_developer
}

fn make_diagnostic(route: &LlmRoute, span: Span) -> LintDiagnostic {
    let route_label = if route.model.is_empty() {
        route.provider.clone()
    } else {
        format!("{}:{}", route.provider, route.model)
    };
    LintDiagnostic {
        code: Code::ReminderUnsupportedUserBlockRoleHint,
        rule: RULE_NAME,
        message: format!(
            "`role_hint: \"user_block\"` is provider-specific, but `{route_label}` cannot render it as an Anthropic user content block or an OpenAI developer message."
        ),
        span,
        severity: LintSeverity::Warning,
        suggestion: Some(
            "use `role_hint: \"system\"` or choose the hint from provider capability flags."
                .to_string(),
        ),
        fix: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_lexer::Lexer;
    use harn_parser::Parser;

    fn lint(source: &str) -> Vec<LintDiagnostic> {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let program = Parser::new(tokens).parse().expect("parse");
        let mut diags = Vec::new();
        check_reminder_role_hint_capabilities(&program, &mut diags);
        diags
    }

    fn count_rule(diags: &[LintDiagnostic]) -> usize {
        diags.iter().filter(|d| d.rule == RULE_NAME).count()
    }

    #[test]
    fn warns_for_user_block_with_local_route() {
        let diags = lint(
            r#"
pipeline default(task) {
    register_reminder_provider({
        id: "custom",
        subscribes_to: ["session_idle"],
        evaluate: { _ctx -> return {reminder: {body: "heads up", role_hint: "user_block"}} },
    })
    llm_call("hi", nil, {provider: "mock", model: "mock"})
}
"#,
        );
        assert_eq!(count_rule(&diags), 1, "diags: {diags:?}");
        let diag = diags.iter().find(|d| d.rule == RULE_NAME).unwrap();
        assert_eq!(diag.code, Code::ReminderUnsupportedUserBlockRoleHint);
        assert_eq!(diag.severity, LintSeverity::Warning);
    }

    #[test]
    fn allows_anthropic_user_block_route() {
        let diags = lint(
            r#"
pipeline default(task) {
    register_reminder_provider({
        id: "custom",
        subscribes_to: ["session_idle"],
        evaluate: { _ctx -> return {reminder: {body: "heads up", role_hint: "user_block"}} },
    })
    llm_call("hi", nil, {provider: "mock", model: "claude-opus-4-20250514"})
}
"#,
        );
        assert_eq!(count_rule(&diags), 0, "diags: {diags:?}");
    }

    #[test]
    fn allows_openai_developer_route() {
        let diags = lint(
            r#"
pipeline default(task) {
    register_reminder_provider({
        id: "custom",
        subscribes_to: ["session_idle"],
        evaluate: { _ctx -> return {reminder: {body: "heads up", role_hint: "user_block"}} },
    })
    llm_call("hi", nil, {provider: "mock", model: "o3"})
}
"#,
        );
        assert_eq!(count_rule(&diags), 0, "diags: {diags:?}");
    }

    #[test]
    fn skips_dynamic_provider_routes() {
        let diags = lint(
            r#"
pipeline default(task) {
    let provider = choose_provider()
    register_reminder_provider({
        id: "custom",
        subscribes_to: ["session_idle"],
        evaluate: { _ctx -> return {reminder: {body: "heads up", role_hint: "user_block"}} },
    })
    llm_call("hi", nil, {provider: provider, model: "mock"})
}
"#,
        );
        assert_eq!(count_rule(&diags), 0, "diags: {diags:?}");
    }
}
