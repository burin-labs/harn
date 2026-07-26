use std::path::Path;

use harn_parser::{DiagnosticCode as Code, Node, SNode};

use super::{dict_literal_field, literal_string, PreflightDiagnostic};

/// Reject literal provider/model/tool-format compositions that the runtime
/// capability registry would have to correct. Dynamic configurations remain
/// runtime-checked; this pass only reports facts it can prove from the source.
///
/// The capability registry remains the sole policy owner. Preflight calls the
/// same `no_viable_tool_channel` / `validate_tool_format` functions used at
/// dispatch instead of maintaining a second table of forbidden combinations.
pub(super) fn scan_llm_tool_format_composition_preflight(
    file_path: &Path,
    source: &str,
    program: &[SNode],
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    for node in program {
        scan_node(node, file_path, source, diagnostics);
    }
}

fn scan_node(
    node: &SNode,
    file_path: &Path,
    source: &str,
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    if let Node::FunctionCall { name, args, .. } = &node.node {
        if let Some(options) = options_arg(name, args) {
            check_literal_composition(name, options, file_path, source, diagnostics);
        }
    }
    for child in harn_parser::visit::immediate_children(node) {
        scan_node(child, file_path, source, diagnostics);
    }
}

/// Return the options argument for public calls that either dispatch a
/// tool-bearing model turn or construct canonical agent-loop options.
fn options_arg<'a>(name: &str, args: &'a [SNode]) -> Option<&'a SNode> {
    let index = match name {
        "agent_options" | "agent_loop_options" | "agent_preset_options" => 0,
        "agent_turn" | "agent_preset" | "agent_governed_preset" => 1,
        "agent_loop"
        | "agent_llm_turn"
        | "agent_stream_call"
        | "llm_call"
        | "llm_call_safe"
        | "llm_call_structured"
        | "llm_call_structured_safe"
        | "llm_call_structured_result"
        | "llm_stream"
        | "llm_stream_call" => 2,
        "llm_completion" => 3,
        _ => return None,
    };
    args.get(index)
}

fn check_literal_composition(
    call_name: &str,
    options: &SNode,
    file_path: &Path,
    source: &str,
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    let Some(raw_model) = dict_literal_field(options, "model").and_then(literal_string) else {
        return;
    };

    // Raw LLM calls only care about a tool format when they offer tools.
    // Agent calls and option constructors feed the tool-bearing agent loop.
    if call_name.starts_with("llm_")
        && !dict_literal_field(options, "tools")
            .is_some_and(|tools| !matches!(&tools.node, Node::NilLiteral))
    {
        return;
    }

    // Agent options expose an audited experiment seam. Raw llm_call options do
    // not, so only honor the reason on surfaces that can emit its transcript
    // event at runtime.
    if !call_name.starts_with("llm_") {
        if let Some(reason) = dict_literal_field(options, "tool_format_override_reason") {
            let Some(reason) = literal_string(reason) else {
                return;
            };
            if !reason.trim().is_empty() {
                return;
            }
        }
    }

    let (resolved_model, alias_provider) = harn_vm::llm_config::resolve_model(&raw_model);
    let provider = match dict_literal_field(options, "provider") {
        Some(provider) => match literal_string(provider) {
            Some(provider) if !provider.eq_ignore_ascii_case("auto") => provider,
            Some(_) => {
                alias_provider.unwrap_or_else(|| harn_vm::llm_config::infer_provider(&raw_model))
            }
            None => return,
        },
        None => alias_provider.unwrap_or_else(|| harn_vm::llm_config::infer_provider(&raw_model)),
    };

    let invalid = if let Some(message) =
        harn_vm::llm::capabilities::no_viable_tool_channel(&provider, &resolved_model)
    {
        Some(message)
    } else {
        let Some(requested) = dict_literal_field(options, "tool_format").and_then(literal_string)
        else {
            return;
        };
        if requested == "auto" {
            return;
        }
        harn_vm::llm::capabilities::validate_tool_format(&provider, &resolved_model, &requested)
            .correction
    };
    let Some(reason) = invalid else {
        return;
    };

    diagnostics.push(PreflightDiagnostic {
        code: Code::LlmToolFormatCompositionInvalid,
        path: file_path.display().to_string(),
        source: source.to_string(),
        span: options.span,
        message: format!(
            "preflight: `{call_name}` requests a known-unsafe LLM composition: {reason}"
        ),
        help: Some(if call_name.starts_with("llm_") {
            "use the catalog-recommended tool_format (or omit it)".to_string()
        } else {
            "use the catalog-recommended tool_format (or omit it), or add a non-empty \
             tool_format_override_reason for a deliberate provider probe"
                .to_string()
        }),
        tags: None,
    });
}
