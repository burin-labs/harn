//! Declaration policy for removable pipeline inputs.

use harn_lexer::{FixEdit, Span};
use harn_parser::TypedParam;

use super::Linter;
use crate::decls::{ParamDeclaration, RemovablePipelineParam};
use crate::fixes::pipeline_parameter_removal_fix;

impl Linter<'_> {
    pub(super) fn declare_pipeline_parameters(
        &mut self,
        params: &[TypedParam],
        owner: &str,
        removal_allowed: bool,
    ) {
        for (index, parameter) in params.iter().enumerate() {
            let removable = removal_allowed
                && !parameter.rest
                && parameter.default_value.is_none()
                && parameter.name.starts_with('_');
            let removal = removable
                .then(|| pipeline_parameter_removal_fix(self.source, params, index))
                .flatten();
            if let Some((fix, fix_after_removed_previous)) = removal {
                self.declare_removable_pipeline_parameter(
                    &parameter.name,
                    parameter.span,
                    owner,
                    fix,
                    index
                        .checked_sub(1)
                        .map(|previous| params[previous].name.clone()),
                    fix_after_removed_previous,
                );
            } else {
                self.declare_parameter(&parameter.name, parameter.span);
                if removal_allowed {
                    self.references.insert(parameter.name.clone());
                }
            }
        }
    }

    /// Declare an explicitly unused test-runner input whose positional slot can
    /// be deleted when no caller reference survives the full walk.
    pub(super) fn declare_removable_pipeline_parameter(
        &mut self,
        name: &str,
        span: Span,
        owner: &str,
        fix: Vec<FixEdit>,
        previous_name: Option<String>,
        fix_after_removed_previous: Option<Vec<FixEdit>>,
    ) {
        if name == "_" {
            return;
        }
        self.warn_if_shadows_outer_scope(name, span);
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string());
        }
        self.param_declarations.push(ParamDeclaration {
            name: name.to_string(),
            span,
            removable_pipeline: Some(RemovablePipelineParam {
                owner: owner.to_string(),
                fix,
                previous_name,
                fix_after_removed_previous,
            }),
        });
    }
}
