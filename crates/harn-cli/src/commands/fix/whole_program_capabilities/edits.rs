//! Project an analyzed capability plan into source edits.

use std::collections::{BTreeMap, BTreeSet};

use harn_builtin_meta::CapabilityId;
use harn_lexer::{FixEdit, Span};
use harn_parser::TypedParam;

use super::super::capability_migrations::{ambient_call_rewrite, ambient_replacement};
use super::super::signature_threading::add_call_argument_edit;
use super::super::CallSite;
use super::{
    ambient_call_capability, diagnostic_capability, CarrierKind, FileDiagnostics, ProgramCallable,
};

pub(super) fn carrier_supplies(kind: &CarrierKind, capability: CapabilityId) -> bool {
    match kind {
        CarrierKind::Root => true,
        CarrierKind::Narrow(current) => *current == capability,
        CarrierKind::Bundle(capabilities) => capabilities.contains(&capability),
    }
}

pub(super) fn split_capability_signature_edit(
    callable: &ProgramCallable,
    additions: &BTreeMap<CapabilityId, String>,
) -> Result<FixEdit, String> {
    let last = callable
        .carriers
        .iter()
        .max_by_key(|carrier| carrier.param_index)
        .ok_or_else(|| format!("{} has no split capability boundary", callable.info.name))?;
    let replacement = additions
        .iter()
        .map(|(capability, name)| format!(", {name}: {}", capability.type_name()))
        .collect::<String>();
    Ok(FixEdit {
        span: Span::with_offsets(
            last.param.span.end,
            last.param.span.end,
            last.param.span.line,
            last.param.span.column,
        ),
        replacement,
    })
}

struct CapabilityAccess<'a> {
    binding: &'a str,
    direct: bool,
}

fn capability_access<'a>(
    callable: &'a ProgramCallable,
    desired: &CarrierKind,
    additions: &'a BTreeMap<CapabilityId, String>,
    capability: CapabilityId,
) -> Option<CapabilityAccess<'a>> {
    if callable.has_split_capability_params {
        if let Some(carrier) = callable
            .carriers
            .iter()
            .filter(|carrier| carrier_supplies(&carrier.kind, capability))
            .min_by_key(|carrier| match carrier.kind {
                CarrierKind::Narrow(_) => 0,
                CarrierKind::Bundle(_) => 1,
                CarrierKind::Root => 2,
            })
        {
            return Some(CapabilityAccess {
                binding: &carrier.name,
                direct: matches!(carrier.kind, CarrierKind::Narrow(_)),
            });
        }
        return additions.get(&capability).map(|binding| CapabilityAccess {
            binding,
            direct: true,
        });
    }
    carrier_supplies(desired, capability).then(|| CapabilityAccess {
        binding: final_binding(callable).unwrap_or("harness"),
        direct: matches!(desired, CarrierKind::Narrow(_)),
    })
}

fn capability_value(
    callable: &ProgramCallable,
    desired: &CarrierKind,
    additions: &BTreeMap<CapabilityId, String>,
    capability: CapabilityId,
) -> Option<String> {
    let access = capability_access(callable, desired, additions, capability)?;
    Some(if access.direct {
        access.binding.to_string()
    } else {
        format!("{}.{}", access.binding, capability.field_name())
    })
}

pub(super) fn argument_for_kind(
    callable: &ProgramCallable,
    desired: &CarrierKind,
    additions: &BTreeMap<CapabilityId, String>,
    required: &CarrierKind,
) -> Result<String, String> {
    match required {
        CarrierKind::Root => {
            if callable.has_split_capability_params {
                callable
                    .carriers
                    .iter()
                    .find(|carrier| matches!(carrier.kind, CarrierKind::Root))
                    .map(|carrier| carrier.name.clone())
                    .ok_or_else(|| "a narrow caller cannot supply root Harness".to_string())
            } else if matches!(desired, CarrierKind::Root) {
                Ok(final_binding(callable).unwrap_or("harness").to_string())
            } else {
                Err("a narrow caller cannot supply root Harness".to_string())
            }
        }
        CarrierKind::Narrow(capability) => {
            capability_value(callable, desired, additions, *capability)
                .ok_or_else(|| "caller does not hold the required capability".to_string())
        }
        CarrierKind::Bundle(required) => {
            let exact_bundle = if callable.has_split_capability_params {
                callable.carriers.iter().find_map(|carrier| {
                    matches!(&carrier.kind, CarrierKind::Bundle(available) if available == required)
                        .then_some(carrier.name.as_str())
                })
            } else {
                matches!(desired, CarrierKind::Bundle(available) if available == required)
                    .then(|| final_binding(callable).unwrap_or("harness"))
            };
            if let Some(binding) = exact_bundle {
                return Ok(binding.to_string());
            }
            let fields = required
                .iter()
                .map(|capability| {
                    capability_value(callable, desired, additions, *capability)
                        .map(|value| format!("{}: {value}", capability.field_name()))
                })
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| "caller does not hold the required capability bundle".to_string())?;
            Ok(format!("{{{}}}", fields.join(", ")))
        }
    }
}

/// Project every capability argument required to make a split call contiguous.
/// A missing ordinary parameter is not synthesizable, so that call is deferred.
pub(super) fn split_call_extension(
    caller: &ProgramCallable,
    caller_desired: &CarrierKind,
    caller_additions: &BTreeMap<CapabilityId, String>,
    callee: &ProgramCallable,
    callee_additions: &BTreeMap<CapabilityId, String>,
    call_arity: usize,
) -> Result<Option<(usize, Vec<String>)>, String> {
    let extension_index = callee
        .carriers
        .iter()
        .map(|carrier| carrier.param_index)
        .max()
        .expect("split callable has capability carriers")
        + 1;
    let insertion_index = call_arity.min(extension_index);
    let mut arguments = Vec::new();
    for param_index in call_arity..extension_index {
        let Some(carrier) = callee
            .carriers
            .iter()
            .find(|carrier| carrier.param_index == param_index)
        else {
            return Ok(None);
        };
        arguments.push(argument_for_kind(
            caller,
            caller_desired,
            caller_additions,
            &carrier.kind,
        )?);
    }
    for capability in callee_additions.keys() {
        arguments.push(
            capability_value(caller, caller_desired, caller_additions, *capability).ok_or_else(
                || {
                    format!(
                        "{} cannot supply {} to {}",
                        caller.info.name,
                        capability.type_name(),
                        callee.info.name
                    )
                },
            )?,
        );
    }
    Ok(Some((insertion_index, arguments)))
}

pub(super) fn signature_edit(
    source: &str,
    callable: &ProgramCallable,
    desired: &CarrierKind,
) -> Result<FixEdit, String> {
    let replacement_type = render_type(desired);
    if let Some(carrier) = &callable.carrier {
        let span = parameter_type_span(source, &carrier.param).ok_or_else(|| {
            format!(
                "failed to locate capability type for {}",
                callable.info.name
            )
        })?;
        return Ok(FixEdit {
            span,
            replacement: replacement_type,
        });
    }
    let name = if !callable.info.bound_names.contains("harness") {
        "harness"
    } else if !callable.info.bound_names.contains("_harness") {
        "_harness"
    } else {
        return Err(format!(
            "{} has no available capability binding",
            callable.info.name
        ));
    };
    Ok(FixEdit {
        span: Span::with_offsets(
            callable.info.insert_offset,
            callable.info.insert_offset,
            callable.info.span.line,
            callable.info.span.column,
        ),
        replacement: if callable.info.has_params {
            format!("{name}: {replacement_type}, ")
        } else {
            format!("{name}: {replacement_type}")
        },
    })
}

fn parameter_type_span(source: &str, param: &TypedParam) -> Option<Span> {
    let region = source.get(param.span.start..param.span.end)?;
    let colon = region.find(':')? + 1;
    let suffix = &region[colon..];
    let end = suffix.find('=').unwrap_or(suffix.len());
    let leading = suffix.len() - suffix.trim_start().len();
    let trimmed_end = suffix[..end].trim_end().len();
    Some(Span::with_offsets(
        param.span.start + colon + leading,
        param.span.start + colon + trimmed_end,
        param.span.line,
        param.span.column,
    ))
}

pub(super) fn receiver_projection_edits(
    callable: &ProgramCallable,
    desired: &CarrierKind,
) -> Vec<FixEdit> {
    let binding = final_binding(callable).unwrap_or("harness");
    let mut edits = Vec::new();
    match (
        callable.carrier.as_ref().map(|carrier| &carrier.kind),
        desired,
    ) {
        (Some(CarrierKind::Root), CarrierKind::Narrow(capability)) => {
            for access in &callable.receiver_accesses {
                if access.property == capability.field_name() {
                    edits.push(FixEdit {
                        span: access.access_span,
                        replacement: binding.to_string(),
                    });
                }
            }
        }
        (Some(CarrierKind::Narrow(capability)), CarrierKind::Bundle(_)) => {
            edits.extend(callable.receiver_accesses.iter().map(|access| FixEdit {
                span: access.object_span,
                replacement: format!("{binding}.{}", capability.field_name()),
            }));
        }
        (None, CarrierKind::Narrow(capability)) => {
            edits.extend(
                callable
                    .receiver_accesses
                    .iter()
                    .filter(|access| access.property == capability.field_name())
                    .map(|access| FixEdit {
                        span: access.access_span,
                        replacement: binding.to_string(),
                    }),
            );
        }
        (None, CarrierKind::Root | CarrierKind::Bundle(_)) if binding != "harness" => {
            edits.extend(callable.receiver_accesses.iter().map(|access| FixEdit {
                span: access.object_span,
                replacement: binding.to_string(),
            }));
        }
        _ => {}
    }
    edits
}

pub(super) fn ambient_edits(
    source: &str,
    callable: &ProgramCallable,
    desired: &CarrierKind,
    additions: &BTreeMap<CapabilityId, String>,
    diagnostics: Option<&FileDiagnostics<'_>>,
) -> Vec<FixEdit> {
    let Some(diagnostics) = diagnostics else {
        return Vec::new();
    };
    let mut edits = Vec::new();
    for ambient in &callable.info.ambient_capability_calls {
        if !diagnostics.ambient_spans.contains(&(
            ambient.code,
            ambient.span.start,
            ambient.span.end,
        )) {
            continue;
        }
        let Some(capability) = ambient_call_capability(ambient) else {
            continue;
        };
        let Some(access) = capability_access(callable, desired, additions, capability) else {
            continue;
        };
        let Some(mut replacement) =
            ambient_replacement(ambient.code, &ambient.name, Some(access.binding))
        else {
            continue;
        };
        if access.direct {
            replacement = replacement.replacen(
                &format!("{}.{}", access.binding, capability.field_name()),
                access.binding,
                1,
            );
        }
        if let Some(mut call_edits) = ambient_call_rewrite(source, ambient, &replacement) {
            edits.append(&mut call_edits);
        }
    }
    edits
}

pub(super) fn explicit_capability_argument_edits(
    source: &str,
    callable: &ProgramCallable,
    desired: &CarrierKind,
    additions: &BTreeMap<CapabilityId, String>,
    diagnostics: Option<&FileDiagnostics<'_>>,
) -> Vec<FixEdit> {
    let Some(diagnostics) = diagnostics else {
        return Vec::new();
    };
    let mut edited_calls = BTreeSet::new();
    let mut edits = Vec::new();
    for diagnostic in &diagnostics.missing_capability_arguments {
        let Some(span) = diagnostic.span else {
            continue;
        };
        if span.start < callable.info.span.start || span.end > callable.info.span.end {
            continue;
        }
        let Some(capability) = diagnostic_capability(diagnostic) else {
            continue;
        };
        let Some(argument) = capability_value(callable, desired, additions, capability) else {
            continue;
        };
        let Some((call, argument_index)) = callable.info.calls.iter().find_map(|call| {
            call.args
                .iter()
                .position(|candidate| candidate.start == span.start && candidate.end == span.end)
                .map(|index| (call, index))
        }) else {
            continue;
        };
        if !edited_calls.insert((call.span.start, call.span.end)) {
            // Argument indexes describe the pre-edit call. Apply at most one
            // insertion to each call per fixed-point pass, then let the next
            // typecheck locate the remaining mismatch in the updated call.
            continue;
        }
        if let Some(edit) = add_call_argument_at_index_edit(source, call, argument_index, &argument)
        {
            edits.push(edit);
        }
    }
    edits
}

fn final_binding(callable: &ProgramCallable) -> Option<&str> {
    callable
        .carrier
        .as_ref()
        .map(|carrier| carrier.name.as_str())
        .or_else(|| {
            (!callable.info.bound_names.contains("harness"))
                .then_some("harness")
                .or_else(|| (!callable.info.bound_names.contains("_harness")).then_some("_harness"))
        })
}

pub(super) fn add_call_argument_at_index_edit(
    source: &str,
    call: &CallSite,
    index: usize,
    argument: &str,
) -> Option<FixEdit> {
    if index == 0 {
        return add_call_argument_edit(source, &call.span, argument);
    }
    let previous = call.args.get(index - 1)?;
    Some(FixEdit {
        span: Span::with_offsets(previous.end, previous.end, call.span.line, call.span.column),
        replacement: format!(", {argument}"),
    })
}

pub(super) fn add_call_arguments_at_index_edit(
    source: &str,
    call: &CallSite,
    index: usize,
    arguments: &[String],
) -> Option<FixEdit> {
    let arguments = arguments.join(", ");
    if index == 0 {
        return add_call_argument_edit(source, &call.span, &arguments);
    }
    let previous = call.args.get(index - 1)?;
    Some(FixEdit {
        span: Span::with_offsets(previous.end, previous.end, call.span.line, call.span.column),
        replacement: format!(", {arguments}"),
    })
}

fn render_type(kind: &CarrierKind) -> String {
    match kind {
        CarrierKind::Root => "Harness".to_string(),
        CarrierKind::Narrow(capability) => capability.type_name().to_string(),
        CarrierKind::Bundle(capabilities) => format!(
            "{{{}}}",
            capabilities
                .iter()
                .map(|capability| {
                    format!("{}: {}", capability.field_name(), capability.type_name())
                })
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}
