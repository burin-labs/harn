//! `template-variant-explosion` lint rule.
//!
//! Warn when a single `.harn.prompt` template has more than `N`
//! capability-aware conditionals. The literature's #1 failure mode
//! for prompt-variant systems is combinatorial explosion + silent
//! drift between materializations — keeping branch count low keeps
//! the rendered prompts comparable.
//!
//! Threshold defaults to `3` and is configurable via
//! `[lint] template_variant_branch_threshold` in `harn.toml`.

use harn_parser::DiagnosticCode as Code;
use harn_vm::stdlib::template::lint::{ConditionShape, LintConstruct};

use crate::diagnostic::{LintDiagnostic, LintSeverity};

pub(crate) const RULE_NAME: &str = "template-variant-explosion";

/// Default threshold; see [`template_variant_branch_threshold`] in
/// `harn_cli::config::LintConfig`. Picked to match the issue's stated
/// default (#1669).
pub const DEFAULT_BRANCH_THRESHOLD: usize = 3;

/// Emit a single diagnostic per template when the count of
/// capability-aware conditional branches exceeds `threshold`. The
/// diagnostic anchors to the first capability branch so the author
/// sees a location to click into.
pub(crate) fn check(
    constructs: &[LintConstruct],
    threshold: usize,
    source: &str,
) -> Vec<LintDiagnostic> {
    let mut capability_branches: Vec<(usize, usize)> = Vec::new();
    for construct in constructs {
        if let LintConstruct::IfChain { branches } = construct {
            for branch in branches {
                if matches!(branch.condition, ConditionShape::CapabilityFlag { .. }) {
                    capability_branches.push((branch.line, branch.col));
                }
            }
        }
    }
    if capability_branches.len() <= threshold {
        return Vec::new();
    }
    let (line, col) = capability_branches.first().copied().unwrap_or((1, 1));
    let span = crate::template_span::directive_span(source, line, col);
    let count = capability_branches.len();
    let message = format!(
        "this template has {count} capability-aware branches but the configured \
         threshold is {threshold}. Combinatorial explosion + drift between \
         materializations is the #1 failure mode for prompt-variant systems — \
         consider splitting into logical sections (`{{{{ section \"task\" }}}}`, \
         `{{{{ section \"tools\" }}}}`, …) so the capability dispatch lives in \
         one shared place instead of being scattered across the prompt.",
    );
    vec![LintDiagnostic {
        code: Code::LintTemplateVariantExplosion,
        rule: RULE_NAME.into(),
        message,
        span,
        severity: LintSeverity::Warning,
        suggestion: Some(
            "Lift each capability branch into a logical section (see \
             docs/src/prompt-templating.md#logical-sections) or raise \
             `[lint] template_variant_branch_threshold` in harn.toml if the \
             count is intentional."
                .to_string(),
        ),
        fix: None,
    }]
}

#[cfg(test)]
mod tests {
    use crate::lint_prompt_template;

    fn rule_count(d: &[crate::LintDiagnostic], rule: &str) -> usize {
        d.iter().filter(|x| x.rule == rule).count()
    }

    fn many_branches(n: usize) -> String {
        (0..n)
            .map(|i| {
                let flag = match i % 4 {
                    0 => "native_tools",
                    1 => "prefers_xml_scaffolding",
                    2 => "supports_assistant_prefill",
                    _ => "prefers_markdown_scaffolding",
                };
                format!("{{{{ if llm.capabilities.{flag} }}}}x{{{{ end }}}}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn under_threshold_no_diag() {
        let d = lint_prompt_template(&many_branches(3), None, &[]);
        assert_eq!(rule_count(&d, super::RULE_NAME), 0);
    }

    #[test]
    fn over_threshold_emits_one_diag() {
        let d = lint_prompt_template(&many_branches(5), None, &[]);
        assert_eq!(rule_count(&d, super::RULE_NAME), 1);
        let diag = d.iter().find(|x| x.rule == super::RULE_NAME).unwrap();
        assert!(diag.message.contains("5 capability-aware branches"));
        assert!(diag.message.contains("threshold is 3"));
    }

    #[test]
    fn configurable_threshold_lifts_signal() {
        // With default threshold 3, 4 branches would fire. With an
        // explicit threshold of 5 the rule should not fire.
        let d = lint_prompt_template(&many_branches(4), Some(5), &[]);
        assert_eq!(rule_count(&d, super::RULE_NAME), 0);
    }

    #[test]
    fn provider_identity_branches_do_not_count_toward_explosion() {
        let identity_branches = (0..5)
            .map(|i| {
                let provider = match i % 3 {
                    0 => "anthropic",
                    1 => "openai",
                    _ => "google",
                };
                format!("{{{{ if llm.provider == \"{provider}\" }}}}x{{{{ end }}}}")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let d = lint_prompt_template(&identity_branches, None, &[]);
        // template-variant-explosion is about capability flags
        // specifically — identity branches get caught by the sibling
        // rule, not double-counted here.
        assert_eq!(rule_count(&d, super::RULE_NAME), 0);
        assert_eq!(
            rule_count(&d, super::super::template_provider_identity::RULE_NAME),
            5,
        );
    }

    #[test]
    fn rule_can_be_disabled() {
        let d = lint_prompt_template(&many_branches(6), None, &[super::RULE_NAME.to_string()]);
        assert_eq!(rule_count(&d, super::RULE_NAME), 0);
    }
}
