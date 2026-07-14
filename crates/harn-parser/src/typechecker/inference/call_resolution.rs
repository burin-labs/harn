//! Cross-module call-target name resolution.

use crate::ast::SNode;
use crate::builtin_signatures;
use crate::diagnostic_codes::Code;
use harn_lexer::Span;

use super::super::scope::{is_builtin, TypeScope};
use super::super::TypeChecker;

impl TypeChecker {
    pub(super) fn check_cross_module_call_target_resolves(
        &mut self,
        name: &str,
        _args: &[SNode],
        scope: &TypeScope,
        span: Span,
    ) {
        let Some(imported) = self.imported_names.as_ref() else {
            return;
        };

        let resolvable = is_builtin(name)
            || name == "schema_of"
            || scope.get_fn(name).is_some()
            || scope.get_struct(name).is_some()
            || scope.get_enum(name).is_some()
            || scope.get_var(name).is_some()
            || imported.contains(name)
            || scope.is_generic_type_param(name)
            || name.starts_with("__")
            || name.starts_with("hostlib_")
            || matches!(name, "Ok" | "Err" | "Some" | "None");
        if resolvable {
            return;
        }

        let candidates: Vec<String> = builtin_signatures::iter_builtin_names()
            .map(str::to_string)
            .chain(scope.all_fn_names())
            .chain(imported.iter().cloned())
            .collect();
        let suggestion = crate::diagnostic::renamed_stdlib_symbol(name)
            .map(str::to_string)
            .or_else(|| {
                crate::diagnostic::find_closest_match(
                    name,
                    candidates.iter().map(|s| s.as_str()),
                    2,
                )
                .map(str::to_string)
            });
        let message = match &suggestion {
            Some(s) => {
                format!("call target `{name}` is not defined or imported — did you mean `{s}`?")
            }
            None => format!("call target `{name}` is not defined or imported"),
        };
        match suggestion {
            Some(s) => self.error_at_with_help(
                Code::UndefinedFunction,
                message,
                span,
                format!("did you mean `{s}`?"),
            ),
            None => self.error_at(Code::UndefinedFunction, message, span),
        }
    }
}
