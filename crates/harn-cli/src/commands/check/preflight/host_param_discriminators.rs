use std::path::Path;

use harn_parser::{DiagnosticCode as Code, Node, SNode};

use super::PreflightDiagnostic;
use crate::commands::check::host_capabilities::HostCapabilities;

pub(in crate::commands::check) fn host_render_path_arg(arg: Option<&SNode>) -> Option<String> {
    let Node::DictLiteral(entries) = &arg?.node else {
        return None;
    };
    entries
        .iter()
        .find_map(|entry| match (&entry.key.node, &entry.value.node) {
            (Node::Identifier(key), Node::StringLiteral(path)) if key == "path" => {
                Some(path.clone())
            }
            (Node::StringLiteral(key), Node::StringLiteral(path)) if key == "path" => {
                Some(path.clone())
            }
            _ => None,
        })
}

pub(in crate::commands::check) fn parse_host_call_args(
    args: &[SNode],
) -> Option<(String, String, Option<&SNode>)> {
    let Node::StringLiteral(name) = &args.first()?.node else {
        return None;
    };
    let (capability, operation) = name.split_once('.')?;
    Some((capability.to_string(), operation.to_string(), args.get(1)))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn scan_host_param_discriminators(
    call_node: &SNode,
    params_arg: Option<&SNode>,
    host_capabilities: &HostCapabilities,
    capability: &str,
    operation: &str,
    file_path: &Path,
    source: &str,
    diagnostics: &mut Vec<PreflightDiagnostic>,
) {
    let Some(discriminators) = host_capabilities.param_discriminators(capability, operation) else {
        return;
    };
    let entries = params_arg.and_then(|arg| match &arg.node {
        Node::DictLiteral(entries) => Some(entries.as_slice()),
        _ => None,
    });
    for (field, policy) in discriminators {
        let field_entry = entries.and_then(|entries| {
            entries.iter().find(|entry| match &entry.key.node {
                Node::Identifier(key) | Node::StringLiteral(key) => key == field,
                _ => false,
            })
        });
        if let Some(entry) = field_entry {
            if let Node::StringLiteral(value) = &entry.value.node {
                if !policy.allowed_values.contains(value) {
                    diagnostics.push(PreflightDiagnostic {
                        code: Code::CapabilityUnknownOperation,
                        path: file_path.display().to_string(),
                        source: source.to_string(),
                        span: entry.value.span,
                        message: format!(
                            "preflight: unknown `{field}` discriminator '{value}' for host capability/operation '{capability}.{operation}'"
                        ),
                        help: Some(format!(
                            "use one of the host-declared literal values: {}",
                            policy
                                .allowed_values
                                .iter()
                                .cloned()
                                .collect::<Vec<_>>()
                                .join(", ")
                        )),
                        tags: Some(format!("{capability}.{operation}")),
                    });
                }
                continue;
            }
            if policy.allow_dynamic {
                continue;
            }
        }
        diagnostics.push(PreflightDiagnostic {
            code: Code::CapabilityCallStaticNameRequired,
            path: file_path.display().to_string(),
            source: source.to_string(),
            span: field_entry
                .map(|entry| entry.value.span)
                .or_else(|| params_arg.map(|arg| arg.span))
                .unwrap_or(call_node.span),
            message: format!(
                "preflight: host_call(\"{capability}.{operation}\", ...) requires a literal `{field}` params field for static validation"
            ),
            help: Some(format!(
                "set `{field}` to one of the host-declared literal values: {}",
                policy
                    .allowed_values
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            tags: Some(format!("{capability}.{operation}")),
        });
    }
}
