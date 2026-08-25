//! Post-walk handling for positional parameters that are not referenced.

use std::collections::HashSet;

use harn_parser::DiagnosticCode as Code;

use super::Linter;
use crate::diagnostic::{LintDiagnostic, LintSeverity};
use crate::fixes::replace_identifier_text_fix;

impl Linter<'_> {
    pub(super) fn finalize_unused_parameters(&mut self) {
        let removable_parameters: HashSet<_> = self
            .param_declarations
            .iter()
            .filter_map(|declaration| {
                let candidate = declaration.removable_pipeline.as_ref()?;
                (!self.references.contains(&declaration.name)
                    && !self.function_references.contains(&candidate.owner))
                .then(|| (candidate.owner.clone(), declaration.name.clone()))
            })
            .collect();

        for decl in &self.param_declarations {
            if (decl.name.starts_with('_') && decl.removable_pipeline.is_none())
                || self.references.contains(&decl.name)
            {
                continue;
            }
            let removable = decl
                .removable_pipeline
                .as_ref()
                .filter(|candidate| !self.function_references.contains(&candidate.owner));
            let (suggestion, fix) = if let Some(candidate) = removable {
                let previous_will_be_removed =
                    candidate.previous_name.as_ref().is_some_and(|previous| {
                        removable_parameters.contains(&(candidate.owner.clone(), previous.clone()))
                    });
                let fix = if previous_will_be_removed {
                    candidate
                        .fix_after_removed_previous
                        .as_ref()
                        .unwrap_or(&candidate.fix)
                } else {
                    &candidate.fix
                };
                (
                    "remove the unused pipeline input; pipelines declare only the inputs they use"
                        .to_string(),
                    Some(fix.clone()),
                )
            } else {
                // Functions, closures, table-driven tests, fixtures, extended
                // pipelines, and referenced pipelines keep their positional
                // contract. A name prefix declares the slot unused.
                (
                    format!(
                        "rename the parameter to `_{}` to preserve positional arity",
                        decl.name
                    ),
                    replace_identifier_text_fix(
                        self.source,
                        decl.span,
                        &decl.name,
                        &format!("_{}", decl.name),
                    ),
                )
            };
            self.diagnostics.push(LintDiagnostic {
                code: Code::LintUnusedParameter,
                rule: "unused-parameter".into(),
                message: format!("parameter `{}` is declared but never used", decl.name),
                span: decl.span,
                severity: LintSeverity::Warning,
                suggestion: Some(suggestion),
                fix,
            });
        }
    }
}
