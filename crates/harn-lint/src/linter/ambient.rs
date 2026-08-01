//! Registry-driven ambient-global diagnostics outside the legacy families.

use harn_lexer::Span;
use harn_parser::diagnostic::{
    harness_clock_replacement, harness_env_replacement, harness_fs_replacement,
    harness_net_replacement, harness_random_replacement, harness_stdio_replacement,
    renamed_stdlib_symbol,
};
use harn_parser::{DiagnosticCode as Code, SNode, TypeExpr, TypedParam};

use super::Linter;
use crate::diagnostic::{LintDiagnostic, LintSeverity};
use crate::fixes::replace_identifier_text_fix;

struct AmbientCapabilityLint<'a> {
    name: &'a str,
    span: Span,
    replacement: Option<&'static str>,
    code: Code,
    rule: &'static str,
    sub_handle: &'static str,
    require_harness_in_scope: bool,
}

fn is_explicit_seeded_random_call(name: &str, arg_count: usize) -> bool {
    matches!(
        (name, arg_count),
        ("random", 1) | ("random_int", 3) | ("random_choice", 2) | ("random_shuffle", 2)
    )
}

impl Linter<'_> {
    pub(super) fn check_renamed_stdlib_symbol(&mut self, name: &str, span: Span) {
        if harness_stdio_replacement(name).is_some() {
            return;
        }
        let Some(replacement) =
            renamed_stdlib_symbol(name).or_else(|| harn_parser::legacy_builtin_alias_target(name))
        else {
            return;
        };
        // An import of the old spelling is the case this rule exists for, so
        // only a definition in this file takes the name back.
        if self
            .fn_declarations
            .iter()
            .any(|declaration| declaration.name == name)
        {
            return;
        }
        self.diagnostics.push(LintDiagnostic {
            code: Code::LintRenamedStdlibSymbol,
            rule: "renamed-stdlib-symbol".into(),
            message: format!("`{name}` was renamed to `{replacement}`"),
            span,
            severity: LintSeverity::Warning,
            suggestion: Some(format!("replace `{name}` with `{replacement}`")),
            fix: replace_identifier_text_fix(self.source, span, name, replacement),
        });
    }

    /// Flag ambient clock builtins (`now_ms`, `monotonic_ms`, `sleep_ms`,
    /// `timestamp`, `elapsed`) so migration can rewrite them to
    /// `harness.clock.*`.
    pub(super) fn check_ambient_clock_builtin(&mut self, name: &str, span: Span) {
        self.check_ambient_capability_builtin(AmbientCapabilityLint {
            name,
            span,
            replacement: harness_clock_replacement(name),
            code: Code::LintAmbientClockBuiltin,
            rule: "ambient-clock-builtin",
            sub_handle: "clock",
            require_harness_in_scope: false,
        });
    }

    /// Flag ambient stdio builtins so migration can rewrite them through the
    /// explicit stdio capability.
    pub(super) fn check_ambient_stdio_builtin(&mut self, name: &str, span: Span) {
        self.check_ambient_capability_builtin(AmbientCapabilityLint {
            name,
            span,
            replacement: harness_stdio_replacement(name),
            code: Code::LintAmbientStdioBuiltin,
            rule: "ambient-stdio-builtin",
            sub_handle: "stdio",
            require_harness_in_scope: false,
        });
    }

    pub(super) fn check_ambient_fs_builtin(&mut self, name: &str, span: Span) {
        self.check_ambient_capability_builtin(AmbientCapabilityLint {
            name,
            span,
            replacement: harness_fs_replacement(name),
            code: Code::LintAmbientFsBuiltin,
            rule: "ambient-fs-builtin",
            sub_handle: "fs",
            require_harness_in_scope: false,
        });
    }

    pub(super) fn check_ambient_env_builtin(&mut self, name: &str, span: Span) {
        self.check_ambient_capability_builtin(AmbientCapabilityLint {
            name,
            span,
            replacement: harness_env_replacement(name),
            code: Code::LintAmbientEnvBuiltin,
            rule: "ambient-env-builtin",
            sub_handle: "env",
            require_harness_in_scope: false,
        });
    }

    /// Seeded RNG calls are deterministic data operations, not ambient host
    /// random access.
    pub(super) fn check_ambient_random_builtin(
        &mut self,
        name: &str,
        arg_count: usize,
        span: Span,
    ) {
        if is_explicit_seeded_random_call(name, arg_count) {
            return;
        }
        self.check_ambient_capability_builtin(AmbientCapabilityLint {
            name,
            span,
            replacement: harness_random_replacement(name),
            code: Code::LintAmbientRandomBuiltin,
            rule: "ambient-random-builtin",
            sub_handle: "random",
            require_harness_in_scope: false,
        });
    }

    pub(super) fn check_ambient_net_builtin(&mut self, name: &str, span: Span) {
        self.check_ambient_capability_builtin(AmbientCapabilityLint {
            name,
            span,
            replacement: harness_net_replacement(name),
            code: Code::LintAmbientNetBuiltin,
            rule: "ambient-net-builtin",
            sub_handle: "net",
            require_harness_in_scope: false,
        });
    }

    fn check_ambient_capability_builtin(&mut self, lint: AmbientCapabilityLint<'_>) {
        let Some(replacement) = lint.replacement else {
            return;
        };
        if self.has_local_or_imported_name(lint.name) {
            return;
        }
        let harness_binding = self.harness_binding_name();
        if lint.require_harness_in_scope && harness_binding.is_none() {
            return;
        }
        let replacement = harness_binding
            .map(|binding| replacement.replacen("harness", binding, 1))
            .unwrap_or_else(|| replacement.to_string());
        let fix = harness_binding.and_then(|_| {
            replace_identifier_text_fix(self.source, lint.span, lint.name, &replacement)
        });
        let suggestion = if harness_binding.is_some() {
            format!("replace `{}` with `{}`", lint.name, replacement)
        } else {
            format!(
                "run `harn fix --apply --safety surface-changing` to thread the explicit capability \
                 parameter required by `{replacement}`"
            )
        };
        self.diagnostics.push(LintDiagnostic {
            code: lint.code,
            rule: lint.rule.into(),
            message: format!(
                "ambient `{}` is deprecated — capabilities now route through `harness.{}.*`",
                lint.name, lint.sub_handle,
            ),
            span: lint.span,
            severity: LintSeverity::Warning,
            suggestion: Some(suggestion),
            fix,
        });
    }

    pub(super) fn harness_binding_name(&self) -> Option<&str> {
        self.harness_param_stack
            .last()
            .and_then(|name| name.as_deref())
    }

    fn has_local_or_imported_name(&self, name: &str) -> bool {
        self.scopes.iter().any(|scope| scope.contains(name))
            || (self.known_functions.contains(name) && !self.builtin_functions.contains(name))
            || self
                .imports
                .iter()
                .any(|import| import.names.iter().any(|imported| imported == name))
            || self
                .fn_declarations
                .iter()
                .any(|declaration| declaration.name == name)
    }

    pub(super) fn callable_harness_param(params: &[TypedParam]) -> Option<String> {
        params.iter().find_map(|param| {
            let TypeExpr::Named(name) = param.type_expr.as_ref()? else {
                return None;
            };
            (name == "Harness" && matches!(param.name.as_str(), "harness" | "_harness"))
                .then(|| param.name.clone())
        })
    }

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

    /// Whether an `import "module"` could be supplying `name`.
    ///
    /// A wildcard import hides the real name set, so a call that looks like a
    /// removed global may be a module function that takes its handle as an
    /// ordinary argument — `std/runtime` exports
    /// `runtime_prompt_content(runtime: HarnessRuntime)`, which reads exactly
    /// like the global it replaced.
    pub(super) fn wildcard_import_may_supply(&self, name: &str) -> bool {
        if self.use_module_graph_for_wildcards {
            return self
                .module_graph_wildcard_exports
                .as_ref()
                .is_none_or(|exports| exports.contains(name));
        }
        self.has_wildcard_import
    }

    /// Catch every remaining migration recipe so new typed capabilities do
    /// not silently lose repair coverage.
    pub(super) fn check_ambient_harness_method(&mut self, name: &str, _args: &[SNode], span: Span) {
        if harness_clock_replacement(name).is_some()
            || harn_parser::builtin_signatures::is_language_intrinsic(name)
            || harness_stdio_replacement(name).is_some()
            || harness_fs_replacement(name).is_some()
            || harness_env_replacement(name).is_some()
            || harness_random_replacement(name).is_some()
            || harness_net_replacement(name).is_some()
            || self.has_local_or_imported_name(name)
            || self.wildcard_import_may_supply(name)
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
