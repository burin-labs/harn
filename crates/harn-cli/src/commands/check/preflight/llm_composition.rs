use std::path::Path;

use harn_parser::{DiagnosticCode as Code, Node, SNode};

use super::super::harness_receiver::harness_method_receiver;
use super::{dict_literal_field, literal_string, PreflightDiagnostic};

/// Reject literal provider/model/option compositions that the runtime
/// capability registry cannot represent. Dynamic routes remain runtime-
/// checked; this pass only reports facts it can prove from the source.
///
/// The capability registry remains the sole policy owner. Preflight calls the
/// same `no_viable_tool_channel` / `validate_tool_format` functions used at
/// dispatch instead of maintaining a second table of forbidden combinations.
pub(super) fn scan_llm_capability_composition_preflight(
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
    // The `llm_*` entries in `options_arg` are removed ambient globals whose
    // supported spelling is `harness.llm.<method>`. Resolving the receiver back
    // to its registry name reuses that one table rather than growing a second
    // one keyed by `(capability, method)`; the `agent_*` entries are `std/agent`
    // module functions and keep matching as plain calls. Registry-backed
    // Harness entries use an internal `__cap_` primary name; stripping that
    // mechanical prefix recovers the existing options-table key without
    // duplicating the method-to-argument-position policy.
    if let Node::MethodCall {
        object,
        method,
        args,
    }
    | Node::OptionalMethodCall {
        object,
        method,
        args,
    } = &node.node
    {
        if let Some(receiver) = harness_method_receiver(object) {
            let builtin =
                harn_vm::stdlib::capability_method_manifest_entry(receiver.capability, method)
                    .map(|entry| entry.canonical_name)
                    .and_then(|name| name.strip_prefix("__cap_"));
            if let Some(options) = builtin.and_then(|name| options_arg(name, args)) {
                // Name the capability path rather than the registry name: it is
                // the spelling at the call site, and the registry name is the
                // removed global the author was told to stop writing.
                check_literal_composition(
                    &format!("harness.{}.{method}", receiver.field),
                    options,
                    file_path,
                    source,
                    diagnostics,
                );
            }
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
        "agent_preset" | "agent_governed_preset" => 1,
        "agent_loop"
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

    for option in harn_vm::llm::capabilities::PortableOption::ALL {
        let Some(value) = dict_literal_field(options, option.name()) else {
            continue;
        };
        if !portable_option_intent_is_static(option, value) {
            continue;
        }
        let admission = if option == harn_vm::llm::capabilities::PortableOption::PromptCacheTtl {
            harn_vm::llm::capabilities::admit_prompt_cache_ttl(
                &provider,
                &resolved_model,
                &literal_string(value).expect("static TTL intent is a string literal"),
            )
        } else {
            harn_vm::llm::capabilities::admit_portable_option(&provider, &resolved_model, option)
        };
        let Err(reason) = admission else {
            continue;
        };
        diagnostics.push(PreflightDiagnostic {
            code: Code::LlmCapabilityCompositionInvalid,
            path: file_path.display().to_string(),
            source: source.to_string(),
            span: value.span,
            message: format!(
                "preflight: `{call_name}` requests a known-unsupported LLM option: {reason}"
            ),
            help: Some(
                "remove the option, choose a compatible route, or use the documented provider_options namespace for a provider-native control"
                    .to_string(),
            ),
            tags: None,
        });
    }

    // Raw LLM calls only care about a tool format when they offer tools.
    // Agent calls and option constructors feed the tool-bearing agent loop.
    if call_name.starts_with("llm_")
        && dict_literal_field(options, "tools")
            .is_none_or(|tools| matches!(&tools.node, Node::NilLiteral))
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

    let invalid_tool_format = if let Some(message) =
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
    let Some(reason) = invalid_tool_format else {
        return;
    };

    diagnostics.push(PreflightDiagnostic {
        code: Code::LlmCapabilityCompositionInvalid,
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

/// Preflight reports only caller intent visible without evaluation. Dynamic
/// values fall through to runtime admission; `cache: false` is an opt-out,
/// not a request for the caching capability.
fn portable_option_intent_is_static(
    option: harn_vm::llm::capabilities::PortableOption,
    value: &SNode,
) -> bool {
    match option {
        harn_vm::llm::capabilities::PortableOption::Cache => {
            matches!(value.node, Node::BoolLiteral(true))
        }
        harn_vm::llm::capabilities::PortableOption::PromptCacheTtl => {
            literal_string(value).is_some()
        }
        _ => matches!(
            value.node,
            Node::StringLiteral(_)
                | Node::RawStringLiteral(_)
                | Node::IntLiteral(_)
                | Node::FloatLiteral(_)
                | Node::BoolLiteral(_)
                | Node::ListLiteral(_)
                | Node::DictLiteral(_)
        ),
    }
}
