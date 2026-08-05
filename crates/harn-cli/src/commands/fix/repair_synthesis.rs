//! Repair synthesis for the Harness capability migration.
//!
//! These build the concrete edit set for a diagnosed capability gap: rewriting
//! an ambient call, inserting a missing capability or root argument, and
//! threading a harness parameter through every caller the migration reaches.
//! They read the parent module's callable graph, so they stay a child of it
//! rather than a sibling with a duplicated view of the same analysis.

use super::*;

pub(super) fn synthesize_ambient_capability_repair(
    diag: &harn_lint::LintDiagnostic,
    source: &str,
    program: &[SNode],
    exported_names: &BTreeSet<String>,
    context: &AmbientRepairContext,
    escape: &mut ValueEscape<'_>,
) -> Option<(Repair, Vec<FixEdit>, RepairImpactWire)> {
    ambient_capability_handle(diag.code)?;
    let infos = collect_callable_infos(program, source, exported_names, escape.referenced_by_value);
    let owner_idx = infos.iter().position(|info| {
        info.ambient_capability_calls.iter().any(|call| {
            call.code == diag.code
                && call.span.start == diag.span.start
                && call.span.end == diag.span.end
        })
    })?;
    let reverse_callers = build_reverse_callers(&infos);
    let owner = &infos[owner_idx];
    let ambient = owner.ambient_capability_calls.iter().find(|call| {
        call.code == diag.code
            && call.span.start == diag.span.start
            && call.span.end == diag.span.end
    })?;
    let replacement_binding = owner
        .harness_binding
        .clone()
        .or_else(|| harness_param_name_for_insert(owner).map(str::to_string));
    let replacement =
        ambient_replacement(diag.code, &ambient.name, replacement_binding.as_deref())?;
    let mut edits = ambient_call_rewrite(source, ambient, &replacement)?;

    if owner.harness_binding.is_some() {
        return Some((
            Repair::from_template(diag.code.repair_template()?),
            edits,
            RepairImpactWire::local_ambient("existing-harness-binding"),
        ));
    }
    let needed = propagate_harness_requirements(&infos, &reverse_callers, owner_idx);
    let primary_call_start = owner
        .ambient_capability_calls
        .iter()
        .filter(|call| call.code == diag.code)
        .map(|call| call.span.start)
        .min()
        .unwrap_or(diag.span.start);
    if diag.span.start != primary_call_start {
        return Some((
            repair_for_ambient_capability_plan(diag.code, &infos, &reverse_callers, &needed)?,
            edits,
            repair_impact_for_signature_threading(
                &infos,
                &needed,
                context.cross_module_importer_count,
            ),
        ));
    }

    for &idx in &needed {
        let info = &infos[idx];
        escape.record(info);
        push_signature_edits(&mut edits, source, info)?;
    }
    for (callee_idx, callers) in reverse_callers.iter().enumerate() {
        if !needed.contains(&callee_idx) {
            continue;
        }
        for &(caller_idx, call_idx) in callers {
            let caller = &infos[caller_idx];
            let arg_name = match caller.harness_binding.as_deref() {
                Some(binding) => binding,
                None if needed.contains(&caller_idx) => harness_param_name_for_insert(caller)?,
                None => continue,
            };
            edits.push(add_call_argument_edit(
                source,
                &caller.calls[call_idx].span,
                arg_name,
            )?);
        }
    }

    Some((
        repair_for_ambient_capability_plan(diag.code, &infos, &reverse_callers, &needed)?,
        dedupe_edits(edits),
        repair_impact_for_signature_threading(&infos, &needed, context.cross_module_importer_count),
    ))
}

pub(super) fn synthesize_missing_capability_argument_repair(
    span: Span,
    expected: &TypeExpr,
    actual: &TypeExpr,
    source: &str,
    program: &[SNode],
) -> Option<(Repair, Vec<FixEdit>, RepairImpactWire)> {
    let expected_name = match expected {
        TypeExpr::Named(name) => Some(name.as_str()),
        _ => None,
    };
    let capability = expected_name.and_then(harn_builtin_meta::CapabilityId::from_type_name);
    let mut matched_argument = None;
    visit::walk_program(program, &mut |node| {
        let Node::FunctionCall { args, .. } = &node.node else {
            return;
        };
        for candidate in args {
            if candidate.span.start != span.start || candidate.span.end != span.end {
                continue;
            }
            // A root grant reaches a call as a bare binding (`harness`) or as a
            // field of one (`request.harness`, `self.deps.harness`). Both are
            // paths with no side effect, so appending the sub-grant is the same
            // structural edit; taking the argument's own source keeps whichever
            // one the caller wrote. Anything else — a call, an index, a
            // conditional — is not safely re-rootable and is left alone.
            if matches!(
                &candidate.node,
                Node::Identifier(_) | Node::PropertyAccess { .. }
            ) {
                matched_argument = source
                    .get(candidate.span.start..candidate.span.end)
                    .map(|text| (candidate.span, text.to_string()));
            }
        }
    });

    // After attenuating a helper signature, turn an existing root grant into
    // the expected sub-grant in place. Appending to a simple identifier is
    // structural and preserves the caller's chosen binding name.
    if matches!(actual, TypeExpr::Named(name) if name == "Harness") {
        let (argument_span, binding) = matched_argument?;
        if let Some(replacement) = capability_bundle_literal(expected, &binding) {
            return Some((
                Repair {
                    id: harn_parser::RepairId::from_owned(
                        "bindings/attenuate-capability-bundle-argument".to_string(),
                    ),
                    summary:
                        "Pass the closed capability bundle required by the attenuated callable"
                            .to_string(),
                    safety: RepairSafety::SurfaceChanging,
                },
                vec![FixEdit {
                    span: argument_span,
                    replacement,
                }],
                RepairImpactWire::local_ambient("attenuate-capability-bundle-argument"),
            ));
        }
        let capability = capability?;
        let expected_name = expected_name?;
        return Some((
            Repair {
                id: harn_parser::RepairId::from_owned(
                    "bindings/attenuate-capability-argument".to_string(),
                ),
                summary: format!(
                    "Pass the `{expected_name}` sub-grant required by the attenuated callable"
                ),
                safety: RepairSafety::SurfaceChanging,
            },
            vec![FixEdit {
                span: argument_span,
                replacement: format!("{binding}.{}", capability.field_name()),
            }],
            RepairImpactWire::local_ambient("attenuate-capability-argument"),
        ));
    }

    let _capability = capability?;
    let expected_name = expected_name?;
    let argument = capability_argument_for_span(program, span, expected_name)?;
    let edit = insert_call_argument_before_span(source, program, span, &argument)?;
    Some((
        Repair {
            id: harn_parser::RepairId::from_owned(
                "bindings/prepend-capability-argument".to_string(),
            ),
            summary: format!(
                "Pass the explicit `{expected_name}` capability required by the migrated callable"
            ),
            safety: RepairSafety::SurfaceChanging,
        },
        vec![edit],
        RepairImpactWire::local_ambient("prepend-capability-argument"),
    ))
}

pub(super) fn synthesize_missing_zero_arg_capability_repair(
    call_span: Span,
    expected: &TypeExpr,
    source: &str,
    program: &[SNode],
) -> Option<(Repair, Vec<FixEdit>, RepairImpactWire)> {
    let TypeExpr::Named(expected_name) = expected else {
        return None;
    };
    harn_builtin_meta::CapabilityId::from_type_name(expected_name)?;
    let argument = capability_argument_for_span(program, call_span, expected_name)?;
    let edit = add_call_argument_edit(source, &call_span, &argument)?;
    Some((
        Repair {
            id: harn_parser::RepairId::from_owned(
                "bindings/prepend-capability-argument".to_string(),
            ),
            summary: format!(
                "Pass the explicit `{expected_name}` capability required by the migrated callable"
            ),
            safety: RepairSafety::SurfaceChanging,
        },
        vec![edit],
        RepairImpactWire::local_ambient("prepend-capability-argument"),
    ))
}

pub(super) fn capability_bundle_literal(expected: &TypeExpr, binding: &str) -> Option<String> {
    let TypeExpr::Shape(fields) = expected else {
        return None;
    };
    let fields = fields
        .iter()
        .map(|field| {
            if field.optional {
                return None;
            }
            let TypeExpr::Named(type_name) = &field.type_expr else {
                return None;
            };
            let capability = harn_builtin_meta::CapabilityId::from_type_name(type_name)?;
            (capability.field_name() == field.name)
                .then(|| format!("{}: {binding}.{}", field.name, field.name))
        })
        .collect::<Option<Vec<_>>>()?;
    (!fields.is_empty()).then(|| format!("{{{}}}", fields.join(", ")))
}

/// Emit the new capability parameter and, when the callable's arity is fixed by
/// a local `type X = fn(...)`, the edit that moves that alias with it.
///
/// The two must land in the same pass. A widened signature without its alias —
/// or an alias without its signature — does not type-check, and `harn fix
/// --apply` runs unattended.
pub(super) fn push_signature_edits(
    edits: &mut Vec<FixEdit>,
    source: &str,
    info: &CallableInfo,
) -> Option<()> {
    edits.push(add_harness_param_edit(source, info)?);
    for alias_edit in &info.alias_widening_edits {
        if !edits.iter().any(|edit| edit.span == alias_edit.span) {
            edits.push(alias_edit.clone());
        }
    }
    Some(())
}

pub(super) fn synthesize_missing_harness_repair(
    span: Span,
    source: &str,
    program: &[SNode],
    exported_names: &BTreeSet<String>,
    context: &AmbientRepairContext,
    escape: &mut ValueEscape<'_>,
) -> Option<(Repair, Vec<FixEdit>, RepairImpactWire)> {
    let infos = collect_callable_infos(program, source, exported_names, escape.referenced_by_value);
    let owner_idx = infos
        .iter()
        .enumerate()
        .filter(|(_, info)| info.span.start <= span.start && info.span.end >= span.end)
        .min_by_key(|(_, info)| info.span.end.saturating_sub(info.span.start))
        .map(|(index, _)| index)?;
    if infos[owner_idx].harness_binding.is_some() {
        return None;
    }
    let reverse_callers = build_reverse_callers(&infos);
    let needed = propagate_harness_requirements(&infos, &reverse_callers, owner_idx);
    let mut edits = Vec::new();
    for &idx in &needed {
        escape.record(&infos[idx]);
        push_signature_edits(&mut edits, source, &infos[idx])?;
    }
    for (callee_idx, callers) in reverse_callers.iter().enumerate() {
        if !needed.contains(&callee_idx) {
            continue;
        }
        for &(caller_idx, call_idx) in callers {
            let caller = &infos[caller_idx];
            let arg_name = match caller.harness_binding.as_deref() {
                Some(binding) => binding,
                None if needed.contains(&caller_idx) => harness_param_name_for_insert(caller)?,
                None => continue,
            };
            edits.push(add_call_argument_edit(
                source,
                &caller.calls[call_idx].span,
                arg_name,
            )?);
        }
    }
    Some((
        Repair {
            id: harn_parser::RepairId::from_owned("bindings/thread-missing-harness".to_string()),
            summary:
                "Thread the explicit Harness grant through this callable and its local callers"
                    .to_string(),
            safety: RepairSafety::SurfaceChanging,
        },
        dedupe_edits(edits),
        repair_impact_for_signature_threading(&infos, &needed, context.cross_module_importer_count),
    ))
}

pub(super) fn synthesize_missing_root_argument_repair(
    span: Span,
    source: &str,
    program: &[SNode],
    exported_names: &BTreeSet<String>,
    context: &AmbientRepairContext,
    escape: &mut ValueEscape<'_>,
) -> Option<(Repair, Vec<FixEdit>, RepairImpactWire)> {
    let infos = collect_callable_infos(program, source, exported_names, escape.referenced_by_value);
    let owner_idx = infos
        .iter()
        .enumerate()
        .filter(|(_, info)| info.span.start <= span.start && info.span.end >= span.end)
        .min_by_key(|(_, info)| info.span.end.saturating_sub(info.span.start))
        .map(|(index, _)| index)?;
    if let Some(owner_binding) = infos[owner_idx].harness_binding.as_deref() {
        let edit = insert_call_argument_before_span(source, program, span, owner_binding)?;
        return Some((
            Repair {
                id: harn_parser::RepairId::from_owned("bindings/thread-root-argument".to_string()),
                summary: "Pass the root Harness required by the migrated callable".to_string(),
                safety: RepairSafety::SurfaceChanging,
            },
            vec![edit],
            RepairImpactWire::local_ambient("existing-root-harness-binding"),
        ));
    }
    let reverse_callers = build_reverse_callers(&infos);
    let needed = propagate_harness_requirements(&infos, &reverse_callers, owner_idx);
    let owner_binding = harness_param_name_for_insert(&infos[owner_idx])?;
    let mut edits = vec![insert_call_argument_before_span(
        source,
        program,
        span,
        owner_binding,
    )?];
    for &idx in &needed {
        if infos[idx].harness_binding.is_none() {
            escape.record(&infos[idx]);
            push_signature_edits(&mut edits, source, &infos[idx])?;
        }
    }
    for (callee_idx, callers) in reverse_callers.iter().enumerate() {
        if !needed.contains(&callee_idx) {
            continue;
        }
        for &(caller_idx, call_idx) in callers {
            let caller = &infos[caller_idx];
            let argument = match caller.harness_binding.as_deref() {
                Some(binding) => binding,
                None if needed.contains(&caller_idx) => harness_param_name_for_insert(caller)?,
                None => continue,
            };
            edits.push(add_call_argument_edit(
                source,
                &caller.calls[call_idx].span,
                argument,
            )?);
        }
    }
    Some((
        Repair {
            id: harn_parser::RepairId::from_owned("bindings/thread-root-argument".to_string()),
            summary: "Thread the root Harness required by the migrated callable".to_string(),
            safety: RepairSafety::SurfaceChanging,
        },
        dedupe_edits(edits),
        repair_impact_for_signature_threading(&infos, &needed, context.cross_module_importer_count),
    ))
}

pub(super) fn repair_impact_for_signature_threading(
    infos: &[CallableInfo],
    needed: &BTreeSet<usize>,
    cross_module_importer_count: usize,
) -> RepairImpactWire {
    let signature_changes = needed
        .iter()
        .map(|&idx| {
            let info = &infos[idx];
            SignatureChangeWire {
                callable: info.name.clone(),
                is_exported: info.is_exported,
                is_entrypoint: info.name == "main",
            }
        })
        .collect::<Vec<_>>();
    RepairImpactWire::signature_threading(signature_changes, cross_module_importer_count)
}
