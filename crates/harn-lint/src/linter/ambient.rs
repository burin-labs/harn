//! Registry-driven ambient-global diagnostics outside the legacy families.

use harn_lexer::Span;
use harn_parser::diagnostic::{
    harness_clock_replacement, harness_env_replacement, harness_fs_replacement,
    harness_net_replacement, harness_random_replacement, harness_stdio_replacement,
};
use harn_parser::{DiagnosticCode as Code, SNode};

use super::Linter;
use crate::diagnostic::{LintDiagnostic, LintSeverity};
use crate::fixes::replace_identifier_text_fix;

impl Linter<'_> {
    pub(super) fn check_interpolated_ambient_calls(
        &mut self,
        segments: &[harn_lexer::StringSegment],
    ) {
        let Some(source) = self.source else {
            return;
        };
        let mut calls = Vec::new();
        for segment in segments {
            let harn_lexer::StringSegment::Expression(expression, line, column) = segment else {
                continue;
            };
            let Some(expression) = harn_parser::interpolation::parse_expression(
                Some(source),
                expression,
                *line,
                *column,
            ) else {
                continue;
            };
            harn_parser::visit::walk_node(&expression, &mut |node| {
                let harn_parser::Node::FunctionCall { name, args, .. } = &node.node else {
                    return;
                };
                calls.push((name.clone(), args.len(), node.span));
            });
        }

        for (name, arg_count, span) in calls {
            self.check_ambient_clock_builtin(&name, span);
            self.check_ambient_stdio_builtin(&name, span);
            self.check_ambient_fs_builtin(&name, span);
            self.check_ambient_env_builtin(&name, span);
            self.check_ambient_random_builtin(&name, arg_count, span);
            self.check_ambient_net_builtin(&name, span);
            self.check_ambient_harness_method(&name, &[], span);
        }
    }

    /// Catch every remaining migration recipe so new typed capabilities do
    /// not silently lose repair coverage.
    pub(super) fn check_ambient_harness_method(&mut self, name: &str, _args: &[SNode], span: Span) {
        if harness_clock_replacement(name).is_some()
            || harness_stdio_replacement(name).is_some()
            || harness_fs_replacement(name).is_some()
            || harness_env_replacement(name).is_some()
            || harness_random_replacement(name).is_some()
            || harness_net_replacement(name).is_some()
            || self.has_local_or_imported_name(name)
        {
            return;
        }
        let Some(migration) = harn_vm::stdlib::harness_migration_for_builtin(name) else {
            return;
        };
        let capability = migration.capability;
        let method = migration.method;
        let harness_binding = self.harness_binding_name();
        let root = harness_binding.unwrap_or("harness");
        let replacement = format!("{root}.{}.{method}", capability.field_name());
        let fix = match migration.arguments {
            harn_vm::stdlib::HarnessBuiltinArgumentMigration::Forward => harness_binding
                .and_then(|_| replace_identifier_text_fix(self.source, span, name, &replacement)),
            harn_vm::stdlib::HarnessBuiltinArgumentMigration::RequestRecord(_)
            | harn_vm::stdlib::HarnessBuiltinArgumentMigration::CallThenProperty(_) => None,
        };
        let suggestion = match migration.arguments {
            harn_vm::stdlib::HarnessBuiltinArgumentMigration::RequestRecord(fields) => format!(
                "run `harn fix --apply --safety surface-changing` to rewrite the positional call as `{replacement}({{{}}})` and thread its explicit capability",
                fields
                    .iter()
                    .map(|field| format!("{field}: ..."))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            harn_vm::stdlib::HarnessBuiltinArgumentMigration::CallThenProperty(property) => {
                format!(
                    "run `harn fix --apply --safety surface-changing` to replace `{name}()` with `{replacement}().{property}` and thread its explicit capability"
                )
            }
            harn_vm::stdlib::HarnessBuiltinArgumentMigration::Forward
                if harness_binding.is_some() =>
            {
                format!("replace `{name}` with `{replacement}`")
            }
            harn_vm::stdlib::HarnessBuiltinArgumentMigration::Forward => format!(
                "run `harn fix --apply --safety surface-changing` to thread the explicit capability required by `harness.{}.{method}`",
                capability.field_name()
            ),
        };
        self.diagnostics.push(LintDiagnostic {
            code: Code::LintAmbientHarnessMethod,
            rule: "ambient-harness-method".into(),
            message: format!(
                "ambient runtime builtin `{name}` was replaced by `harness.{}.{method}`",
                capability.field_name()
            ),
            span,
            severity: LintSeverity::Warning,
            suggestion: Some(suggestion),
            fix,
        });
    }
}
