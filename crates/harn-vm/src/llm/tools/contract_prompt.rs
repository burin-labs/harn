use std::collections::BTreeMap;
use std::rc::Rc;

use super::collect::{collect_tool_schemas_with_registry, ToolSchema};
use super::params::ToolParamSchema;
use super::type_expr::{ObjectField, TypeExpr};
use crate::value::VmValue;

/// Build a runtime-owned tool-calling contract prompt.
/// The runtime injects this block so prompt templates do not need to carry
/// stale tool syntax examples that can drift from actual parser behavior.
///
/// Rust supplies structured tool schema bindings; the stdlib prompt asset owns
/// the user-visible prose and section layout.
pub(crate) fn build_tool_calling_contract_prompt(
    tools_val: Option<&VmValue>,
    native_tools: Option<&[serde_json::Value]>,
    mode: &str,
    require_action: bool,
    tool_examples: Option<&str>,
    include_task_ledger_help: bool,
    done_sentinel: &str,
) -> String {
    let mut bindings = BTreeMap::new();
    bindings.insert("mode".to_string(), vm_string(mode));
    bindings.insert("native_mode".to_string(), VmValue::Bool(mode == "native"));
    bindings.insert("require_action".to_string(), VmValue::Bool(require_action));
    bindings.insert(
        "include_task_ledger_help".to_string(),
        VmValue::Bool(include_task_ledger_help),
    );
    bindings.insert(
        "tool_examples".to_string(),
        vm_string(tool_examples.map(str::trim).unwrap_or_default()),
    );
    bindings.insert("done_sentinel".to_string(), vm_string(done_sentinel));

    if mode != "native" {
        let (schemas, registry) = collect_tool_schemas_with_registry(tools_val, native_tools);
        let aliases = registry.render_aliases();
        let (expanded, compact): (Vec<_>, Vec<_>) =
            schemas.iter().partition(|schema| !schema.compact);
        bindings.insert("shared_types".to_string(), vm_string(&aliases));
        bindings.insert(
            "expanded_schemas".to_string(),
            vm_string(&render_schema_list(&expanded, false)),
        );
        bindings.insert(
            "compact_schemas".to_string(),
            vm_string(&render_schema_list(&compact, true)),
        );
    }

    crate::stdlib::template::render_stdlib_prompt_asset(
        "agent/prompts/tool_contract_text.harn.prompt",
        Some(&bindings),
    )
    .unwrap_or_else(|error| format!("Tool contract prompt render error: {error}"))
}

fn vm_string(value: &str) -> VmValue {
    VmValue::String(Rc::from(value))
}

fn render_schema_list(schemas: &[&ToolSchema], compact: bool) -> String {
    let mut rendered = String::new();
    for schema in schemas {
        if compact {
            rendered.push_str(&render_compact_text_tool_schema(schema));
        } else {
            rendered.push_str(&render_text_tool_schema(schema));
        }
    }
    rendered
}

fn render_text_tool_schema(schema: &ToolSchema) -> String {
    let mut rendered = String::new();
    let args_type = build_tool_args_type(&schema.params);
    rendered.push_str(&format!(
        "declare function {}(args: {}): string;\n",
        schema.name,
        args_type.render()
    ));
    if !schema.description.trim().is_empty() {
        rendered.push_str("/**\n");
        for line in schema.description.lines() {
            rendered.push_str(&format!(" * {line}\n"));
        }
        rendered.push_str(" */\n");
    }
    rendered.push('\n');
    rendered
}

fn render_compact_text_tool_schema(schema: &ToolSchema) -> String {
    let args_type = build_tool_args_type(&schema.params);
    let summary = schema
        .description
        .split(&['.', '\n'][..])
        .next()
        .unwrap_or("")
        .trim();
    format!(
        "- `{}({})` — {}\n",
        schema.name,
        args_type.render(),
        summary,
    )
}

/// Build the single-arg TypeScript object type that a tool takes. Each
/// top-level parameter becomes a field in the object (optional via `?`, with
/// a JSDoc @example rendered by the containing comment block), with required
/// fields listed first for consistency with the per-param comment order.
fn build_tool_args_type(params: &[ToolParamSchema]) -> TypeExpr {
    let fields: Vec<ObjectField> = params
        .iter()
        .map(|param| ObjectField {
            name: param.name.clone(),
            ty: param.ty.clone(),
            required: param.required,
            description: if param.description.is_empty() {
                None
            } else {
                Some(param.description.clone())
            },
            default: param.default.clone(),
            examples: param.examples.clone(),
        })
        .collect();
    TypeExpr::Object(fields)
}

/// Build the text-mode response protocol help block, substituting the
/// configured sentinel value (or omitting the sentinel guidance entirely
/// when the sentinel is opted out).
#[cfg(test)]
pub(crate) fn text_response_protocol_help(done_sentinel: &str) -> String {
    let mut bindings = BTreeMap::new();
    bindings.insert("done_sentinel".to_string(), vm_string(done_sentinel));
    crate::stdlib::template::render_stdlib_prompt_asset(
        "agent/prompts/tool_contract_text_response_protocol.harn.prompt",
        Some(&bindings),
    )
    .unwrap_or_else(|error| format!("Response protocol prompt render error: {error}"))
}

pub(crate) fn text_response_protocol_repair_feedback(
    protocol_violations: &[String],
    done_sentinel: &str,
) -> String {
    let done_line = if done_sentinel.is_empty() {
        String::new()
    } else {
        format!("<done>{done_sentinel}</done>\n")
    };
    let violations = protocol_violations.join("\n- ");
    let mut bindings = BTreeMap::new();
    bindings.insert("violations".to_string(), vm_string(&violations));
    bindings.insert("done_line".to_string(), vm_string(&done_line));
    crate::stdlib::template::render_stdlib_prompt_asset(
        "agent/prompts/protocol_violation_feedback.harn.prompt",
        Some(&bindings),
    )
    .unwrap_or_else(|error| format!("protocol violation feedback prompt render error: {error}"))
}
