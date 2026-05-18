//! Reminder lifecycle lint rules.

use harn_lexer::Span;
use harn_parser::visit;
use harn_parser::{DiagnosticCode as Code, DictEntry, Node, SNode};

use crate::diagnostic::{LintDiagnostic, LintSeverity};

const RULE_NAME: &str = "reminder-infinite-discardable";

pub(crate) fn check_reminder_lifecycle_literals(
    program: &[SNode],
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    visit::walk_program(program, &mut |node| match &node.node {
        Node::DictLiteral(entries)
        | Node::StructConstruct {
            fields: entries, ..
        } => check_reminder_entries(entries, diagnostics),
        _ => {}
    });
}

fn check_reminder_entries(entries: &[DictEntry], diagnostics: &mut Vec<LintDiagnostic>) {
    if !looks_like_reminder(entries) {
        return;
    }
    let Some(preserve) = entry_for_key(entries, "preserve_on_compact") else {
        return;
    };
    if !matches!(preserve.value.node, Node::BoolLiteral(false)) {
        return;
    }
    if entry_for_key(entries, "ttl_turns")
        .is_some_and(|entry| !matches!(entry.value.node, Node::NilLiteral))
    {
        return;
    }

    diagnostics.push(make_diagnostic(preserve.value.span));
}

fn looks_like_reminder(entries: &[DictEntry]) -> bool {
    entry_for_key(entries, "body").is_some()
        && entry_for_key(entries, "preserve_on_compact").is_some()
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

fn make_diagnostic(span: Span) -> LintDiagnostic {
    LintDiagnostic {
        code: Code::ReminderInfiniteDiscardable,
        rule: RULE_NAME,
        message:
            "`preserve_on_compact: false` with no finite `ttl_turns` can leave a reminder alive indefinitely until compaction drops it."
                .to_string(),
        span,
        severity: LintSeverity::Warning,
        suggestion: Some(
            "add a finite `ttl_turns` or set `preserve_on_compact: true` for durable reminders."
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
        check_reminder_lifecycle_literals(&program, &mut diags);
        diags
    }

    fn count_rule(diags: &[LintDiagnostic]) -> usize {
        diags.iter().filter(|d| d.rule == RULE_NAME).count()
    }

    #[test]
    fn warns_for_discardable_reminder_without_ttl() {
        let diags = lint(
            r#"
pipeline default() {
    transcript.inject_reminder(transcript(), {body: "heads up", preserve_on_compact: false})
}
"#,
        );
        assert_eq!(count_rule(&diags), 1, "diags: {diags:?}");
        assert_eq!(diags[0].code, Code::ReminderInfiniteDiscardable);
        assert_eq!(diags[0].severity, LintSeverity::Warning);
    }

    #[test]
    fn warns_for_discardable_reminder_with_nil_ttl() {
        let diags = lint(
            r#"
pipeline default() {
    let effect = {reminder: {body: "heads up", ttl_turns: nil, preserve_on_compact: false}}
}
"#,
        );
        assert_eq!(count_rule(&diags), 1, "diags: {diags:?}");
    }

    #[test]
    fn allows_finite_ttl() {
        let diags = lint(
            r#"
pipeline default() {
    let effect = {reminder: {body: "heads up", ttl_turns: 2, preserve_on_compact: false}}
}
"#,
        );
        assert_eq!(count_rule(&diags), 0, "diags: {diags:?}");
    }

    #[test]
    fn allows_preserved_reminder_without_ttl() {
        let diags = lint(
            r#"
pipeline default() {
    let effect = {reminder: {body: "heads up", preserve_on_compact: true}}
}
"#,
        );
        assert_eq!(count_rule(&diags), 0, "diags: {diags:?}");
    }
}
