use harn_lexer::Span;
use harn_parser::{DiagnosticCode as Code, Node, SNode};

use super::Linter;
use crate::diagnostic::{LintDiagnostic, LintSeverity};

impl Linter<'_> {
    pub(super) fn check_harness_method_effect_policy(
        &mut self,
        object: &SNode,
        method: &str,
        args: &[SNode],
        span: Span,
    ) {
        let connector_denial =
            self.connector_effect_export_stack
                .last()
                .cloned()
                .and_then(|export| {
                    let harness_name = self.harness_binding_name()?;
                    let capability = match &object.node {
                        Node::PropertyAccess {
                            object: root,
                            property,
                        }
                        | Node::OptionalPropertyAccess {
                            object: root,
                            property,
                        } if matches!(
                            &root.node,
                            Node::Identifier(name) if name == harness_name
                        ) =>
                        {
                            property.clone()
                        }
                        _ => return None,
                    };
                    harn_vm::connector_export_denied_harness_method_reason(
                        &export,
                        &capability,
                        method,
                    )
                    .map(|reason| (export, capability, reason))
                });

        if let Some((export, capability, reason)) = connector_denial {
            self.diagnostics.push(LintDiagnostic {
                code: Code::LintConnectorEffectPolicy,
                rule: "connector-effect-policy".into(),
                message: format!(
                    "connector export `{export}` calls disallowed capability \
                     `harness.{capability}.{method}`: {reason}"
                ),
                span,
                severity: LintSeverity::Warning,
                suggestion: Some(format!(
                    "move `harness.{capability}.{method}` out of `{export}` or configure \
                     a trusted connector effect-policy override"
                )),
                fix: None,
            });
        }

        self.check_redundant_clone_args(method, args);
    }
}
