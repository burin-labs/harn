//! `reminder-provider-count` rule: flag agent loops that enable many reminder
//! providers and risk inflating ambient context.

use std::collections::BTreeSet;

use harn_lexer::Span;
use harn_parser::visit;
use harn_parser::{DiagnosticCode as Code, DictEntry, Node, SNode};

use crate::diagnostic::{LintDiagnostic, LintSeverity};

const RULE_NAME: &str = "reminder-provider-count";
const MAX_ENABLED_PROVIDERS: usize = 8;
const CANONICAL_PROVIDERS: &[&str] = &[
    "token_pressure",
    "idle_nudge",
    "tool_output_truncated",
    "post_compact_recap",
];

pub(crate) fn check_reminder_provider_count(
    program: &[SNode],
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    visit::walk_program(program, &mut |node| {
        let Node::FunctionCall { name, args, .. } = &node.node else {
            return;
        };
        if name != "agent_loop" {
            return;
        }
        for arg in args {
            if let Node::DictLiteral(entries) = &arg.node {
                check_agent_loop_options(entries, diagnostics);
            }
        }
    });
}

fn check_agent_loop_options(entries: &[DictEntry], diagnostics: &mut Vec<LintDiagnostic>) {
    let Some(reminders_entry) = entry_for_key(entries, "reminders") else {
        return;
    };
    let Some((providers, span)) = enabled_provider_ids(&reminders_entry.value) else {
        return;
    };
    if providers.len() <= MAX_ENABLED_PROVIDERS {
        return;
    }
    diagnostics.push(make_diagnostic(providers.len(), span));
}

fn enabled_provider_ids(reminders: &SNode) -> Option<(BTreeSet<String>, Span)> {
    match &reminders.node {
        Node::BoolLiteral(false) | Node::NilLiteral => None,
        Node::ListLiteral(items) => {
            let mut enabled = canonical_provider_ids();
            apply_provider_list(&mut enabled, items);
            Some((enabled, reminders.span))
        }
        Node::DictLiteral(entries) => {
            if entry_for_key(entries, "enabled")
                .is_some_and(|entry| matches!(entry.value.node, Node::BoolLiteral(false)))
            {
                return None;
            }
            let mut enabled = canonical_provider_ids();
            let providers_entry = entry_for_key(entries, "providers")?;
            let Node::ListLiteral(items) = &providers_entry.value.node else {
                return None;
            };
            apply_provider_list(&mut enabled, items);
            Some((enabled, providers_entry.value.span))
        }
        _ => None,
    }
}

fn canonical_provider_ids() -> BTreeSet<String> {
    CANONICAL_PROVIDERS
        .iter()
        .map(|provider| (*provider).to_string())
        .collect()
}

fn apply_provider_list(enabled: &mut BTreeSet<String>, items: &[SNode]) {
    for item in items {
        let Some(raw) = literal_string(item) else {
            continue;
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(id) = trimmed.strip_prefix('-') {
            enabled.remove(id);
        } else {
            enabled.insert(trimmed.to_string());
        }
    }
}

fn entry_for_key<'a>(entries: &'a [DictEntry], key: &str) -> Option<&'a DictEntry> {
    entries
        .iter()
        .find(|entry| key_name(&entry.key).as_deref() == Some(key))
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

fn make_diagnostic(count: usize, span: Span) -> LintDiagnostic {
    LintDiagnostic {
        code: Code::ReminderProviderBloat,
        rule: RULE_NAME,
        message: format!(
            "`agent_loop` enables {count} distinct reminder providers; this can add overlapping ambient context."
        ),
        span,
        severity: LintSeverity::Info,
        suggestion: Some(
            "disable providers that this loop does not need, or split the loop into narrower stages."
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
        check_reminder_provider_count(&program, &mut diags);
        diags
    }

    fn count_rule(diags: &[LintDiagnostic]) -> usize {
        diags.iter().filter(|d| d.rule == RULE_NAME).count()
    }

    #[test]
    fn reports_large_provider_sets_as_info() {
        let diags = lint(
            r#"
pipeline default(task) {
  agent_loop(task, nil, {
    reminders: {providers: ["a", "b", "c", "d", "e"]},
  })
}
"#,
        );
        assert_eq!(count_rule(&diags), 1, "diags: {diags:?}");
        let diag = diags.iter().find(|d| d.rule == RULE_NAME).unwrap();
        assert_eq!(diag.code, Code::ReminderProviderBloat);
        assert_eq!(diag.severity, LintSeverity::Info);
    }

    #[test]
    fn honors_disabled_canonical_providers() {
        let diags = lint(
            r#"
pipeline default(task) {
  agent_loop(task, nil, {
    reminders: {
      providers: ["-token_pressure", "-idle_nudge", "-tool_output_truncated", "-post_compact_recap", "a", "b", "c", "d", "e"],
    },
  })
}
"#,
        );
        assert_eq!(count_rule(&diags), 0, "diags: {diags:?}");
    }

    #[test]
    fn skips_disabled_reminders() {
        let diags = lint(
            r#"
pipeline default(task) {
  agent_loop(task, nil, {reminders: false})
}
"#,
        );
        assert_eq!(count_rule(&diags), 0, "diags: {diags:?}");
    }
}
