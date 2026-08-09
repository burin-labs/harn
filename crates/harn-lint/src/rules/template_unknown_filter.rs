//! `template-unknown-filter` lint rule.
//!
//! Filter names are syntactically identifiers, so the template parser accepts
//! a typo and the renderer cannot reject it until runtime. Validate parsed
//! filter uses against the same registry the renderer dispatches through.

use harn_lexer::FixEdit;
use harn_parser::{diagnostic::find_closest_match, DiagnosticCode as Code};
use harn_vm::stdlib::template::{filters, lint::LintConstruct};

use crate::diagnostic::{LintDiagnostic, LintSeverity};

pub(crate) const RULE_NAME: &str = "template-unknown-filter";

pub(crate) fn check(constructs: &[LintConstruct], source: &str) -> Vec<LintDiagnostic> {
    constructs
        .iter()
        .filter_map(|construct| {
            let LintConstruct::Filter { name, start, end } = construct else {
                return None;
            };
            if filters::lookup(name).is_some() {
                return None;
            }
            let span = crate::template_span::byte_span(source, *start, *end);
            let closest =
                find_closest_match(name, filters::FILTERS.iter().map(|filter| filter.name), 2);
            let suggestion = closest.map(|candidate| format!("did you mean `{candidate}`?"));
            let message = match &suggestion {
                Some(suggestion) => format!("unknown template filter `{name}`; {suggestion}"),
                None => format!("unknown template filter `{name}`"),
            };
            Some(LintDiagnostic {
                code: Code::LintTemplateUnknownFilter,
                rule: RULE_NAME.into(),
                message,
                span,
                severity: LintSeverity::Error,
                suggestion,
                fix: closest.map(|replacement| {
                    vec![FixEdit {
                        span,
                        replacement: replacement.to_string(),
                    }]
                }),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::lint_prompt_template;
    use harn_vm::stdlib::template::filters;

    fn unknown_filters(source: &str) -> Vec<crate::LintDiagnostic> {
        lint_prompt_template(source, None, &[])
            .into_iter()
            .filter(|diagnostic| diagnostic.rule == super::RULE_NAME)
            .collect()
    }

    #[test]
    #[expect(
        clippy::string_slice,
        reason = "diagnostic spans are template-lexer byte offsets into the fixture"
    )]
    fn typo_points_at_the_name_and_offers_a_fix() {
        let source = "héllo {{ name | uppr }}\n";
        let diagnostics = unknown_filters(source);
        assert_eq!(diagnostics.len(), 1);
        let diagnostic = &diagnostics[0];
        assert_eq!(&source[diagnostic.span.start..diagnostic.span.end], "uppr");
        assert_eq!(diagnostic.severity, crate::LintSeverity::Error);
        assert!(diagnostic.message.contains("did you mean `upper`?"));
        let fix = diagnostic
            .fix
            .as_ref()
            .expect("near miss should be fixable");
        assert_eq!(fix.len(), 1);
        assert_eq!(fix[0].replacement, "upper");
    }

    #[test]
    fn every_registered_filter_is_accepted() {
        for filter in filters::FILTERS {
            let source = format!("{{{{ value | {} }}}}", filter.name);
            assert!(
                unknown_filters(&source).is_empty(),
                "registered filter `{}` was rejected",
                filter.name
            );
        }
    }

    #[test]
    fn unrelated_name_is_an_error_without_an_invented_fix() {
        let diagnostics = unknown_filters("{{ value | completely_unknown }}");
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].fix.is_none());
        assert!(diagnostics[0].suggestion.is_none());
    }

    #[test]
    fn rule_can_be_disabled() {
        let diagnostics =
            lint_prompt_template("{{ value | uppr }}", None, &[super::RULE_NAME.to_string()]);
        assert!(diagnostics.is_empty());
    }
}
