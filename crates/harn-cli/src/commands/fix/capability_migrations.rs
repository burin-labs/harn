//! Canonical ambient-global to typed-Harness migration recipes.

use harn_lexer::{FixEdit, Span};
use harn_parser::{visit, DiagnosticCode as Code, Node, SNode};

use super::{replace_identifier_within_span_fix, AmbientCapabilityCall, CallSite};

pub(super) fn ambient_capability_handle(code: Code) -> Option<&'static str> {
    match code {
        Code::LintAmbientClockBuiltin => Some("clock"),
        Code::LintAmbientStdioBuiltin => Some("stdio"),
        Code::LintAmbientFsBuiltin => Some("fs"),
        Code::LintAmbientEnvBuiltin => Some("env"),
        Code::LintAmbientRandomBuiltin => Some("random"),
        Code::LintAmbientNetBuiltin => Some("net"),
        Code::LintAmbientHarnessMethod => Some(""),
        _ => None,
    }
}

pub(super) fn ambient_code_for_call(name: &str, arg_count: usize) -> Option<Code> {
    use harn_parser::diagnostic as replacements;
    if replacements::harness_clock_replacement(name).is_some() {
        return Some(Code::LintAmbientClockBuiltin);
    }
    if replacements::harness_stdio_replacement(name).is_some() {
        return Some(Code::LintAmbientStdioBuiltin);
    }
    if replacements::harness_fs_replacement(name).is_some() {
        return Some(Code::LintAmbientFsBuiltin);
    }
    if replacements::harness_env_replacement(name).is_some() {
        return Some(Code::LintAmbientEnvBuiltin);
    }
    if replacements::harness_random_replacement(name).is_some()
        && !is_explicit_seeded_random_call(name, arg_count)
    {
        return Some(Code::LintAmbientRandomBuiltin);
    }
    if replacements::harness_net_replacement(name).is_some() {
        return Some(Code::LintAmbientNetBuiltin);
    }
    harn_vm::stdlib::harness_migration_for_builtin(name).map(|_| Code::LintAmbientHarnessMethod)
}

fn is_explicit_seeded_random_call(name: &str, arg_count: usize) -> bool {
    matches!(
        (name, arg_count),
        ("random", 1) | ("random_int", 3) | ("random_choice", 2) | ("random_shuffle", 2)
    )
}

pub(super) fn ambient_replacement(code: Code, name: &str, binding: Option<&str>) -> Option<String> {
    use harn_parser::diagnostic as replacements;
    let replacement = match code {
        Code::LintAmbientClockBuiltin => replacements::harness_clock_replacement(name)?.to_string(),
        Code::LintAmbientStdioBuiltin => replacements::harness_stdio_replacement(name)?.to_string(),
        Code::LintAmbientFsBuiltin => replacements::harness_fs_replacement(name)?.to_string(),
        Code::LintAmbientEnvBuiltin => replacements::harness_env_replacement(name)?.to_string(),
        Code::LintAmbientRandomBuiltin => {
            replacements::harness_random_replacement(name)?.to_string()
        }
        Code::LintAmbientNetBuiltin => replacements::harness_net_replacement(name)?.to_string(),
        Code::LintAmbientHarnessMethod => {
            let migration = harn_vm::stdlib::harness_migration_for_builtin(name)?;
            format!(
                "harness.{}.{}",
                migration.capability.field_name(),
                migration.method
            )
        }
        _ => return None,
    };
    Some(replacement.replacen("harness", binding.unwrap_or("harness"), 1))
}

pub(super) fn ambient_call_rewrite(
    source: &str,
    ambient: &AmbientCapabilityCall,
    replacement: &str,
) -> Option<Vec<FixEdit>> {
    if ambient.code != Code::LintAmbientHarnessMethod {
        return replace_identifier_within_span_fix(
            source,
            ambient.span,
            &ambient.name,
            replacement,
        );
    }
    let migration = harn_vm::stdlib::harness_migration_for_builtin(&ambient.name)?;
    match migration.arguments {
        harn_vm::stdlib::HarnessBuiltinArgumentMigration::Forward => {
            replace_identifier_within_span_fix(source, ambient.span, &ambient.name, replacement)
        }
        harn_vm::stdlib::HarnessBuiltinArgumentMigration::RequestRecord(fields) => {
            if ambient.args.len() > fields.len() {
                return None;
            }
            let entries = fields
                .iter()
                .zip(&ambient.args)
                .map(|(field, span)| {
                    Some(format!("{field}: {}", source.get(span.start..span.end)?))
                })
                .collect::<Option<Vec<_>>>()?;
            Some(vec![FixEdit {
                span: ambient.span,
                replacement: format!("{replacement}({{{}}})", entries.join(", ")),
            }])
        }
        harn_vm::stdlib::HarnessBuiltinArgumentMigration::CallThenProperty(property) => {
            if !ambient.args.is_empty() {
                return None;
            }
            Some(vec![FixEdit {
                span: ambient.span,
                replacement: format!("{replacement}().{property}"),
            }])
        }
    }
}

pub(super) fn collect_callable_node_calls(
    node: &SNode,
    source: &str,
    calls: &mut Vec<CallSite>,
    ambient_calls: &mut Vec<AmbientCapabilityCall>,
) {
    if let Node::FunctionCall { name, args, .. } = &node.node {
        record_callable_call(name, args, node.span, calls, ambient_calls);
        return;
    }
    let Node::InterpolatedString(segments) = &node.node else {
        return;
    };
    for segment in segments {
        let harn_lexer::StringSegment::Expression(expression, line, column) = segment else {
            continue;
        };
        let Some(expression) =
            harn_parser::interpolation::parse_expression(Some(source), expression, *line, *column)
        else {
            continue;
        };
        visit::walk_node(&expression, &mut |child| {
            let Node::FunctionCall { name, args, .. } = &child.node else {
                return;
            };
            record_callable_call(name, args, child.span, calls, ambient_calls);
        });
    }
}

fn record_callable_call(
    name: &str,
    args: &[SNode],
    span: Span,
    calls: &mut Vec<CallSite>,
    ambient_calls: &mut Vec<AmbientCapabilityCall>,
) {
    calls.push(CallSite {
        callee: name.to_string(),
        span,
    });
    if let Some(code) = ambient_code_for_call(name, args.len()) {
        ambient_calls.push(AmbientCapabilityCall {
            name: name.to_string(),
            code,
            span,
            args: args.iter().map(|arg| arg.span).collect(),
        });
    }
}
