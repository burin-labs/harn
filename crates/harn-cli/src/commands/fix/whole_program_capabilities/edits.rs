//! Render an analyzed capability plan as source edits.

use std::collections::BTreeSet;
use std::path::Path;

use harn_builtin_meta::CapabilityId;
use harn_lexer::{FixEdit, Span};
use harn_parser::{DiagnosticCode as Code, TypeExpr, TypedParam};

use super::super::capability_migrations::{ambient_call_rewrite, ambient_replacement};
use super::super::signature_threading::add_call_argument_edit;
use super::super::{CallSite, RepairCandidate};
use super::{canonical, is_ambient_code, CarrierKind, ProgramCallable};

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
    diagnostics: &[RepairCandidate],
    file: &Path,
) -> Vec<FixEdit> {
    let binding = final_binding(callable).unwrap_or("harness");
    let diagnostic_spans = diagnostics
        .iter()
        .filter(|diagnostic| {
            is_ambient_code(diagnostic.code) && canonical(Path::new(&diagnostic.file)) == file
        })
        .filter_map(|diagnostic| {
            diagnostic
                .span
                .map(|span| (diagnostic.code, span.start, span.end))
        })
        .collect::<BTreeSet<_>>();
    let mut edits = Vec::new();
    for ambient in &callable.info.ambient_capability_calls {
        if !diagnostic_spans.contains(&(ambient.code, ambient.span.start, ambient.span.end)) {
            continue;
        }
        let Some(mut replacement) = ambient_replacement(ambient.code, &ambient.name, Some(binding))
        else {
            continue;
        };
        if let CarrierKind::Narrow(capability) = desired {
            replacement = replacement.replacen(
                &format!("{binding}.{}", capability.field_name()),
                binding,
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
    diagnostics: &[RepairCandidate],
    file: &Path,
) -> Vec<FixEdit> {
    let Some(binding) = final_binding(callable) else {
        return Vec::new();
    };
    let mut edited_calls = BTreeSet::new();
    let mut edits = Vec::new();
    for diagnostic in diagnostics.iter().filter(|diagnostic| {
        is_missing_capability_argument(diagnostic) && canonical(Path::new(&diagnostic.file)) == file
    }) {
        let Some(span) = diagnostic.span else {
            continue;
        };
        if span.start < callable.info.span.start || span.end > callable.info.span.end {
            continue;
        }
        let Some(capability) = diagnostic_capability(diagnostic) else {
            continue;
        };
        let argument = match desired {
            CarrierKind::Narrow(current) if *current == capability => binding.to_string(),
            CarrierKind::Root | CarrierKind::Bundle(_) => {
                format!("{binding}.{}", capability.field_name())
            }
            CarrierKind::Narrow(_) => continue,
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

pub(super) fn is_missing_capability_argument(diagnostic: &RepairCandidate) -> bool {
    diagnostic.code == Code::ArgumentTypeMismatch
        && matches!(
            diagnostic.repair.id.as_str(),
            "bindings/prepend-capability-argument" | "bindings/thread-root-argument"
        )
}

pub(super) fn diagnostic_capability(diagnostic: &RepairCandidate) -> Option<CapabilityId> {
    let TypeExpr::Named(expected) = diagnostic.expected_type.as_ref()? else {
        return None;
    };
    CapabilityId::from_type_name(expected)
}

pub(super) fn final_binding(callable: &ProgramCallable) -> Option<&str> {
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

pub(super) fn argument_projection(
    binding: &str,
    caller: &CarrierKind,
    callee: &CarrierKind,
) -> Result<String, String> {
    match callee {
        CarrierKind::Root => match caller {
            CarrierKind::Root => Ok(binding.to_string()),
            _ => Err("a narrow caller cannot supply root Harness".to_string()),
        },
        CarrierKind::Narrow(capability) => match caller {
            CarrierKind::Root | CarrierKind::Bundle(_) => {
                Ok(format!("{binding}.{}", capability.field_name()))
            }
            CarrierKind::Narrow(current) if current == capability => Ok(binding.to_string()),
            CarrierKind::Narrow(_) => {
                Err("caller does not hold the required capability".to_string())
            }
        },
        CarrierKind::Bundle(required) => match caller {
            CarrierKind::Root => Ok(render_bundle_value(binding, required)),
            CarrierKind::Bundle(available) if available == required => Ok(binding.to_string()),
            CarrierKind::Bundle(available) if required.is_subset(available) => {
                Ok(render_bundle_value(binding, required))
            }
            _ => Err("caller does not hold the required capability bundle".to_string()),
        },
    }
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

fn render_bundle_value(binding: &str, capabilities: &BTreeSet<CapabilityId>) -> String {
    format!(
        "{{{}}}",
        capabilities
            .iter()
            .map(|capability| {
                let field = capability.field_name();
                format!("{field}: {binding}.{field}")
            })
            .collect::<Vec<_>>()
            .join(", ")
    )
}
