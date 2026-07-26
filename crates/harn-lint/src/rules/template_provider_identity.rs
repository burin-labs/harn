//! `template-provider-identity-branch` lint rule.
//!
//! Warn when a `.harn.prompt` template branches on `llm.provider`,
//! `llm.model`, or `llm.family` directly. Identity-string branches
//! are the vendor-lock trap every framework before #1663 fell into —
//! the rule nudges authors toward capability-flag dispatch instead.
//!
//! The diagnostic surfaces a recommended capability-flag replacement
//! in its body (e.g. `llm.provider == "anthropic"` →
//! `llm.capabilities.prefers_xml_scaffolding`), but no autofix is
//! attached — the mapping from identity-string to capability isn't
//! 1-to-1 enough to apply blindly; the author has to pick the flag
//! that matches the branch's intent.

use harn_parser::DiagnosticCode as Code;
use harn_vm::stdlib::template::lint::{ConditionShape, IdentityField, LintConstruct};

use crate::diagnostic::{LintDiagnostic, LintSeverity};

pub(crate) const RULE_NAME: &str = "template-provider-identity-branch";

/// Walk parsed template constructs and emit one diagnostic per
/// identity-string branch.
pub(crate) fn check(constructs: &[LintConstruct], source: &str) -> Vec<LintDiagnostic> {
    let mut diagnostics = Vec::new();
    for construct in constructs {
        let LintConstruct::IfChain { branches } = construct else {
            continue;
        };
        for branch in branches {
            let ConditionShape::ProviderIdentity(field) = &branch.condition else {
                continue;
            };
            diagnostics.push(make_diagnostic(*field, branch.line, branch.col, source));
        }
    }
    diagnostics
}

fn make_diagnostic(field: IdentityField, line: usize, col: usize, source: &str) -> LintDiagnostic {
    let span = crate::template_span::directive_span(source, line, col);
    let suggestion_text = suggestion(field);
    let message = format!(
        "branching on `llm.{}` couples the template to vendor identity strings — \
         dispatch on a capability flag instead so the prompt stays correct as \
         models and routing layers change (see harn#1663). {}",
        field.as_str(),
        suggestion_text,
    );
    LintDiagnostic {
        code: Code::LintTemplateProviderIdentityBranch,
        rule: RULE_NAME.into(),
        message,
        span,
        severity: LintSeverity::Warning,
        suggestion: Some(suggestion_text),
        // Identity-string comparisons span an open question: which
        // capability flag captures the intent? We expose the
        // recommended replacement in the diagnostic body, but the
        // mapping isn't 1-to-1 enough to make an autofix safe — author
        // must pick the flag that matches the branch's actual purpose.
        fix: None,
    }
}

fn suggestion(field: IdentityField) -> String {
    match field {
        IdentityField::Provider => {
            "For provider==\"anthropic\" replace with `llm.capabilities.prefers_xml_scaffolding`. \
             For provider==\"openai\" replace with `llm.capabilities.prefers_markdown_scaffolding` \
             or `llm.capabilities.prefers_role_developer`. For local/qwen routes use \
             `llm.capabilities.text_tool_wire_format_supported` or \
             `llm.capabilities.native_tools`."
                .to_string()
        }
        IdentityField::Model => {
            "Model-string branches don't survive routing/aliasing. If the branch tracks a \
             real capability difference, use the corresponding `llm.capabilities.<flag>`; \
             otherwise lift the decision out of the template (`agent_preset` / `llm_call` \
             options handle it once)."
                .to_string()
        }
        IdentityField::Family => {
            "Family branches are still vendor-lock: `gpt-5.4` and `gpt-4o` share a family \
             but have different capability profiles. Dispatch on the specific \
             `llm.capabilities.<flag>` you care about instead."
                .to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::lint_prompt_template;

    fn diags(src: &str) -> Vec<crate::LintDiagnostic> {
        lint_prompt_template(src, None, &[])
    }

    fn rule_count(d: &[crate::LintDiagnostic], rule: &str) -> usize {
        d.iter().filter(|x| x.rule == rule).count()
    }

    #[test]
    fn provider_equality_triggers_one_diag() {
        let d = diags("{{ if llm.provider == \"anthropic\" }}x{{ end }}");
        assert_eq!(rule_count(&d, super::RULE_NAME), 1);
        assert!(d[0].message.contains("prefers_xml_scaffolding"));
    }

    #[test]
    fn model_inequality_triggers_diag() {
        let d = diags("{{ if llm.model != \"gpt-5\" }}x{{ end }}");
        assert_eq!(rule_count(&d, super::RULE_NAME), 1);
        assert!(d[0].message.contains("`llm.model`"));
    }

    #[test]
    fn family_branch_triggers_diag() {
        let d = diags("{{ if llm.family == \"anthropic-claude\" }}x{{ end }}");
        assert_eq!(rule_count(&d, super::RULE_NAME), 1);
        assert!(d[0].message.contains("`llm.family`"));
    }

    #[test]
    fn capability_branch_is_not_flagged() {
        let d = diags("{{ if llm.capabilities.native_tools }}x{{ end }}");
        assert_eq!(rule_count(&d, super::RULE_NAME), 0);
    }

    #[test]
    fn elif_chain_flags_each_identity_branch() {
        let d = diags(
            "{{ if llm.provider == \"openai\" }}o\
             {{ elif llm.model == \"gpt-5\" }}g\
             {{ else }}x{{ end }}",
        );
        assert_eq!(rule_count(&d, super::RULE_NAME), 2);
    }

    #[test]
    fn rule_can_be_disabled() {
        let d = lint_prompt_template(
            "{{ if llm.provider == \"anthropic\" }}x{{ end }}",
            None,
            &[super::RULE_NAME.to_string()],
        );
        assert_eq!(rule_count(&d, super::RULE_NAME), 0);
    }
}
