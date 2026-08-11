//! Post-walk handling for positional parameters that are not referenced.

use harn_parser::DiagnosticCode as Code;

use super::Linter;
use crate::diagnostic::{LintDiagnostic, LintSeverity};
use crate::fixes::replace_identifier_text_fix;

impl Linter<'_> {
    pub(super) fn finalize_unused_parameters(&mut self) {
        for decl in &self.param_declarations {
            if decl.name.starts_with('_') || self.references.contains(&decl.name) {
                continue;
            }
            // Arguments are positional: `_` removes a runtime slot, while a
            // name prefix preserves arity and any generated capability intent.
            let fix = replace_identifier_text_fix(
                self.source,
                decl.span,
                &decl.name,
                &format!("_{}", decl.name),
            );
            self.diagnostics.push(LintDiagnostic {
                code: Code::LintUnusedParameter,
                rule: "unused-parameter".into(),
                message: format!("parameter `{}` is declared but never used", decl.name),
                span: decl.span,
                severity: LintSeverity::Warning,
                suggestion: Some(format!(
                    "rename the parameter to `_{}` to preserve positional arity",
                    decl.name
                )),
                fix,
            });
        }
    }
}
