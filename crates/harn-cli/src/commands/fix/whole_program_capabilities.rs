//! Plan capability-carrier signatures and calls for the complete invocation.
//!
//! The per-file linter remains the diagnostic owner. This module turns those
//! diagnostics into one program-wide edit graph so a narrowed signature and
//! every reachable caller move in the same apply pass.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use harn_builtin_meta::CapabilityId;
use harn_lexer::{FixEdit, Span};
use harn_parser::{
    visit, DiagnosticCode as Code, Node, Repair, RepairSafety, SNode, TypeExpr, TypedParam,
};

use super::capability_migrations::{
    ambient_call_rewrite, ambient_capability_handle, ambient_replacement,
};
use super::signature_threading::{add_call_argument_edit, collect_callable_infos};
use super::{CallableInfo, RepairCandidate, RepairImpactWire, SignatureChangeWire};

#[derive(Debug, Clone, PartialEq, Eq)]
enum CarrierKind {
    Root,
    Narrow(CapabilityId),
    Bundle(BTreeSet<CapabilityId>),
}

#[derive(Debug, Clone)]
struct Carrier {
    name: String,
    param_index: usize,
    param: TypedParam,
    kind: CarrierKind,
}

#[derive(Debug)]
struct ProgramFile {
    path: PathBuf,
    source: String,
}

#[derive(Debug)]
struct ProgramCallable {
    file_idx: usize,
    info: CallableInfo,
    receiver_accesses: Vec<ReceiverAccess>,
    boundary: bool,
    flow_predicate: bool,
    carrier: Option<Carrier>,
    root_attenuation: Option<BTreeSet<CapabilityId>>,
    direct_requirements: BTreeSet<CapabilityId>,
}

#[derive(Debug)]
struct ReceiverAccess {
    object_span: Span,
    access_span: Span,
    property: String,
}

#[derive(Debug, Default)]
struct FileDiagnostics<'a> {
    ambient_spans: BTreeSet<(Code, usize, usize)>,
    missing_capability_arguments: Vec<&'a RepairCandidate>,
    representative_ambient_code: Option<Code>,
}

#[derive(Debug, Clone, Copy)]
struct ProgramEdge {
    caller: usize,
    call_idx: usize,
    callee: usize,
}

pub(super) fn plan(
    files: &[PathBuf],
    module_graph: &harn_modules::ModuleGraph,
    diagnostics: &[RepairCandidate],
) -> Result<Vec<RepairCandidate>, String> {
    let mut program_files = Vec::new();
    let mut callables = Vec::new();
    for file in files {
        let source = std::fs::read_to_string(file)
            .map_err(|error| format!("failed to read {}: {error}", file.display()))?;
        let program = harn_parser::parse_source(&source)
            .map_err(|errors| format!("failed to parse {}: {errors:?}", file.display()))?;
        let file_idx = program_files.len();
        let exported = module_graph
            .exports_for_module(file)
            .into_iter()
            .collect::<BTreeSet<_>>();
        let root_attenuations = harn_lint::capability_attenuations(&program)
            .into_iter()
            .map(|candidate| {
                (
                    (
                        candidate.declaration_span.start,
                        candidate.declaration_span.end,
                    ),
                    candidate.capabilities,
                )
            })
            .collect::<BTreeMap<_, _>>();
        let infos = collect_callable_infos(&program, &source, &exported);
        for info in infos {
            let Some((params, body, boundary, flow_predicate)) =
                declaration_parts(&program, info.span)
            else {
                continue;
            };
            let carrier = capability_carrier(params);
            let root_attenuation = root_attenuations
                .get(&(info.span.start, info.span.end))
                .cloned();
            let direct_requirements =
                direct_requirements(params, body, carrier.as_ref(), root_attenuation.as_ref());
            let receiver_accesses = collect_receiver_accesses(body, carrier.as_ref());
            callables.push(ProgramCallable {
                file_idx,
                info,
                receiver_accesses,
                boundary,
                flow_predicate,
                carrier,
                root_attenuation,
                direct_requirements,
            });
        }
        program_files.push(ProgramFile {
            path: canonical(file),
            source,
        });
    }
    if callables.is_empty() {
        return Ok(Vec::new());
    }

    seed_ambient_requirements(&program_files, &mut callables, diagnostics);
    let edges = resolve_edges(&program_files, &callables, module_graph);
    let mut requirements = callables
        .iter()
        .map(|callable| callable.direct_requirements.clone())
        .collect::<Vec<_>>();
    propagate_requirements(&edges, &mut requirements);

    for (callable, required) in callables.iter().zip(&requirements) {
        if !callable.flow_predicate {
            continue;
        }
        let unsupported = required
            .iter()
            .filter(|capability| **capability != CapabilityId::Ast)
            .map(|capability| capability.field_name())
            .collect::<Vec<_>>();
        if !unsupported.is_empty() {
            return Err(format!(
                "flow predicate `{}` requires unsupported injected capabilities: {}; flow evaluation injects only HarnessAst",
                callable.info.name,
                unsupported.join(", ")
            ));
        }
    }

    let desired = callables
        .iter()
        .enumerate()
        .map(|(idx, callable)| desired_carrier(callable, &requirements[idx]))
        .collect::<Vec<_>>();
    let changed = callables
        .iter()
        .zip(&desired)
        .map(|(callable, desired)| carrier_changed(callable.carrier.as_ref(), desired.as_ref()))
        .collect::<Vec<_>>();
    if !changed.iter().any(|changed| *changed)
        && !callables
            .iter()
            .any(|callable| !callable.info.ambient_capability_calls.is_empty())
        && !diagnostics.iter().any(is_missing_capability_argument)
    {
        return Ok(Vec::new());
    }
    let crosses_module_boundary = edges.iter().any(|edge| {
        changed[edge.callee] && callables[edge.caller].file_idx != callables[edge.callee].file_idx
    });
    let surface_changing = crosses_module_boundary
        || callables.iter().enumerate().any(|(idx, callable)| {
            changed[idx]
                && (callable.info.is_exported || (callable.boundary && callable.carrier.is_none()))
        });
    let repair_safety = if surface_changing {
        RepairSafety::SurfaceChanging
    } else {
        RepairSafety::ScopeLocal
    };

    let diagnostics_by_file = diagnostic_index(diagnostics);
    let mut edits_by_file: BTreeMap<usize, Vec<FixEdit>> = BTreeMap::new();
    for (idx, callable) in callables.iter().enumerate() {
        let Some(desired) = desired[idx].as_ref() else {
            continue;
        };
        if changed[idx] {
            edits_by_file
                .entry(callable.file_idx)
                .or_default()
                .push(signature_edit(
                    &program_files[callable.file_idx].source,
                    callable,
                    desired,
                )?);
            edits_by_file
                .entry(callable.file_idx)
                .or_default()
                .extend(receiver_projection_edits(callable, desired));
        }
        edits_by_file
            .entry(callable.file_idx)
            .or_default()
            .extend(ambient_edits(
                &program_files[callable.file_idx].source,
                callable,
                desired,
                diagnostics_by_file.get(&program_files[callable.file_idx].path),
            ));
        edits_by_file.entry(callable.file_idx).or_default().extend(
            explicit_capability_argument_edits(
                &program_files[callable.file_idx].source,
                callable,
                desired,
                diagnostics_by_file.get(&program_files[callable.file_idx].path),
            ),
        );
    }
    for edge in &edges {
        if !changed[edge.callee] {
            continue;
        }
        let callee = &callables[edge.callee];
        let caller = &callables[edge.caller];
        let Some(callee_desired) = desired[edge.callee].as_ref() else {
            continue;
        };
        let Some(caller_desired) = desired[edge.caller].as_ref() else {
            continue;
        };
        let Some(binding) = final_binding(caller) else {
            continue;
        };
        let argument = argument_projection(binding, caller_desired, callee_desired)?;
        let call = &caller.info.calls[edge.call_idx];
        let edit = if let Some(carrier) = &callee.carrier {
            if let Some(argument_span) = call.args.get(carrier.param_index).copied() {
                FixEdit {
                    span: argument_span,
                    replacement: argument,
                }
            } else if call.args.len() == carrier.param_index {
                add_call_argument_at_index_edit(
                    &program_files[caller.file_idx].source,
                    call,
                    carrier.param_index,
                    &argument,
                )
                .ok_or_else(|| format!("failed to update call to {}", callee.info.name))?
            } else {
                return Err(format!(
                    "{} requires capability argument {} after {} omitted positional arguments",
                    callee.info.name,
                    carrier.param_index,
                    carrier.param_index - call.args.len()
                ));
            }
        } else {
            add_call_argument_edit(
                &program_files[caller.file_idx].source,
                &call.span,
                &argument,
            )
            .ok_or_else(|| format!("failed to update call to {}", callee.info.name))?
        };
        edits_by_file.entry(caller.file_idx).or_default().push(edit);
    }

    let mut planned = Vec::new();
    for (file_idx, edits) in edits_by_file {
        let edits = dedupe(edits);
        if edits.is_empty() {
            continue;
        }
        let path = program_files[file_idx].path.to_string_lossy().into_owned();
        let code = diagnostics_by_file
            .get(&program_files[file_idx].path)
            .and_then(|diagnostics| diagnostics.representative_ambient_code)
            .unwrap_or(Code::LintBroadHarnessParameter);
        let signatures = callables
            .iter()
            .enumerate()
            .filter(|(idx, callable)| callable.file_idx == file_idx && changed[*idx])
            .map(|(_, callable)| SignatureChangeWire {
                callable: callable.info.name.clone(),
                is_exported: callable.info.is_exported,
                is_entrypoint: callable.boundary,
            })
            .collect::<Vec<_>>();
        let changes_public_signature = signatures.iter().any(|change| change.is_exported);
        let classification = if changes_public_signature {
            "public-signature-change"
        } else if crosses_module_boundary {
            "whole-program-capability-change"
        } else if signatures.is_empty() {
            "scope-local"
        } else {
            "local-signature-threading"
        };
        planned.push(RepairCandidate {
            file: path,
            source: "whole-program",
            severity: "warning",
            code,
            message: "thread the least capability authority through the invocation graph"
                .to_string(),
            unresolved_name: None,
            expected_type: None,
            span: edits.first().map(|edit| edit.span),
            repair: Repair {
                id: harn_parser::RepairId::from_owned(
                    "bindings/thread-harness-whole-program".to_string(),
                ),
                summary: "Update capability signatures and all reachable call sites together"
                    .to_string(),
                safety: repair_safety,
            },
            impact: RepairImpactWire {
                classification: classification.to_string(),
                strategy: Some("whole-program-fixpoint".to_string()),
                signature_changes: signatures,
                requires_cross_module_caller_updates: crosses_module_boundary,
                notes: vec![
                    "requirements were propagated across resolved module imports; cross-module callers must be updated in the same apply pass"
                        .to_string(),
                ],
            },
            edits,
        });
    }
    Ok(planned)
}

fn declaration_parts(
    program: &[SNode],
    span: Span,
) -> Option<(&[TypedParam], &[SNode], bool, bool)> {
    for node in program {
        let (attributes, inner) = harn_parser::peel_attributes(node);
        if inner.span.start != span.start || inner.span.end != span.end {
            continue;
        }
        let flow_predicate = harn_parser::is_flow_predicate_declaration(attributes, inner);
        return match &inner.node {
            Node::FnDecl {
                name, params, body, ..
            }
            | Node::ToolDecl {
                name, params, body, ..
            } => Some((params, body, name == "main", flow_predicate)),
            Node::Pipeline { params, body, .. } => Some((params, body, true, flow_predicate)),
            _ => None,
        };
    }
    None
}

fn collect_receiver_accesses(body: &[SNode], carrier: Option<&Carrier>) -> Vec<ReceiverAccess> {
    let receiver = carrier.map_or("harness", |carrier| carrier.name.as_str());
    let mut accesses = Vec::new();
    visit::walk_program(body, &mut |node| {
        let (Node::PropertyAccess { object, property }
        | Node::OptionalPropertyAccess { object, property }) = &node.node
        else {
            return;
        };
        if matches!(&object.node, Node::Identifier(name) if name == receiver) {
            accesses.push(ReceiverAccess {
                object_span: object.span,
                access_span: node.span,
                property: property.clone(),
            });
        }
    });
    accesses
}

fn capability_carrier(params: &[TypedParam]) -> Option<Carrier> {
    params.iter().enumerate().find_map(|(param_index, param)| {
        let kind = match param.type_expr.as_ref()? {
            TypeExpr::Named(name) if name == "Harness" => CarrierKind::Root,
            TypeExpr::Named(name) => CarrierKind::Narrow(CapabilityId::from_type_name(name)?),
            TypeExpr::Shape(fields) => {
                let capabilities = fields
                    .iter()
                    .map(|field| match &field.type_expr {
                        TypeExpr::Named(name) => CapabilityId::from_type_name(name),
                        _ => None,
                    })
                    .collect::<Option<BTreeSet<_>>>()?;
                (!capabilities.is_empty()).then_some(CarrierKind::Bundle(capabilities))?
            }
            _ => return None,
        };
        Some(Carrier {
            name: param.name.clone(),
            param_index,
            param: param.clone(),
            kind,
        })
    })
}

fn direct_requirements(
    params: &[TypedParam],
    body: &[SNode],
    carrier: Option<&Carrier>,
    root_attenuation: Option<&BTreeSet<CapabilityId>>,
) -> BTreeSet<CapabilityId> {
    let Some(carrier) = carrier else {
        return BTreeSet::new();
    };
    if matches!(&carrier.kind, CarrierKind::Root) && root_attenuation.is_none() {
        return BTreeSet::new();
    }
    let mut required = match &carrier.kind {
        CarrierKind::Narrow(capability) => BTreeSet::from([*capability]),
        CarrierKind::Bundle(capabilities) => capabilities.clone(),
        CarrierKind::Root => root_attenuation.cloned().unwrap_or_default(),
    };
    let mut observe = |node: &SNode| {
        let (Node::PropertyAccess { object, property }
        | Node::OptionalPropertyAccess { object, property }) = &node.node
        else {
            return;
        };
        if matches!(&object.node, Node::Identifier(name) if name == &carrier.name) {
            if let Some(capability) = CapabilityId::from_field_name(property) {
                required.insert(capability);
            }
        }
    };
    for param in params {
        if let Some(default) = &param.default_value {
            visit::walk_node(default, &mut observe);
        }
    }
    visit::walk_program(body, &mut observe);
    required
}

fn seed_ambient_requirements(
    files: &[ProgramFile],
    callables: &mut [ProgramCallable],
    diagnostics: &[RepairCandidate],
) {
    let undefined_harness_by_file = diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.code == Code::UndefinedVariable
                && diagnostic.unresolved_name.as_deref() == Some("harness")
        })
        .filter_map(|diagnostic| {
            diagnostic.span.map(|span| {
                (
                    canonical(Path::new(&diagnostic.file)),
                    (span.start, span.end),
                )
            })
        })
        .fold(
            BTreeMap::<PathBuf, BTreeSet<(usize, usize)>>::new(),
            |mut by_file, (file, span)| {
                by_file.entry(file).or_default().insert(span);
                by_file
            },
        );
    for callable in callables
        .iter_mut()
        .filter(|callable| callable.carrier.is_none())
    {
        let undefined = undefined_harness_by_file.get(&files[callable.file_idx].path);
        callable.receiver_accesses.retain(|access| {
            undefined.is_some_and(|spans| {
                spans.contains(&(access.object_span.start, access.object_span.end))
            })
        });
        callable.direct_requirements.extend(
            callable
                .receiver_accesses
                .iter()
                .filter_map(|access| CapabilityId::from_field_name(&access.property)),
        );
    }

    for diagnostic in diagnostics {
        if !is_ambient_code(diagnostic.code) {
            continue;
        }
        let Some(span) = diagnostic.span else {
            continue;
        };
        let path = canonical(Path::new(&diagnostic.file));
        let Some((file_idx, _)) = files.iter().enumerate().find(|(_, file)| file.path == path)
        else {
            continue;
        };
        for callable in callables
            .iter_mut()
            .filter(|callable| callable.file_idx == file_idx)
        {
            let Some(call) = callable.info.ambient_capability_calls.iter().find(|call| {
                call.span.start == span.start
                    && call.span.end == span.end
                    && call.code == diagnostic.code
            }) else {
                continue;
            };
            let capability = ambient_capability_handle(call.code)
                .filter(|field| !field.is_empty())
                .and_then(CapabilityId::from_field_name)
                .or_else(|| {
                    harn_vm::stdlib::harness_migration_for_builtin(&call.name)
                        .map(|migration| migration.capability)
                });
            if let Some(capability) = capability {
                callable.direct_requirements.insert(capability);
            }
        }
    }

    for diagnostic in diagnostics {
        if !is_missing_capability_argument(diagnostic) {
            continue;
        }
        let Some(span) = diagnostic.span else {
            continue;
        };
        let Some(capability) = diagnostic_capability(diagnostic) else {
            continue;
        };
        let path = canonical(Path::new(&diagnostic.file));
        let Some((file_idx, _)) = files.iter().enumerate().find(|(_, file)| file.path == path)
        else {
            continue;
        };
        if let Some(callable) = callables
            .iter_mut()
            .filter(|callable| {
                callable.file_idx == file_idx
                    && callable.info.span.start <= span.start
                    && callable.info.span.end >= span.end
            })
            .min_by_key(|callable| {
                callable
                    .info
                    .span
                    .end
                    .saturating_sub(callable.info.span.start)
            })
        {
            callable.direct_requirements.insert(capability);
        }
    }
}

fn resolve_edges(
    files: &[ProgramFile],
    callables: &[ProgramCallable],
    module_graph: &harn_modules::ModuleGraph,
) -> Vec<ProgramEdge> {
    let by_file_name = callables
        .iter()
        .enumerate()
        .map(|(idx, callable)| {
            (
                (
                    files[callable.file_idx].path.clone(),
                    callable.info.name.clone(),
                ),
                idx,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut edges = Vec::new();
    for (caller_idx, caller) in callables.iter().enumerate() {
        let caller_path = &files[caller.file_idx].path;
        for (call_idx, call) in caller.info.calls.iter().enumerate() {
            let target = module_graph
                .definition_of(caller_path, &call.callee)
                .and_then(|definition| {
                    by_file_name
                        .get(&(canonical(&definition.file), definition.name))
                        .copied()
                });
            if let Some(callee) = target {
                edges.push(ProgramEdge {
                    caller: caller_idx,
                    call_idx,
                    callee,
                });
            }
        }
    }
    edges
}

fn propagate_requirements(edges: &[ProgramEdge], requirements: &mut [BTreeSet<CapabilityId>]) {
    let mut callers_by_callee = vec![BTreeSet::new(); requirements.len()];
    for edge in edges {
        callers_by_callee[edge.callee].insert(edge.caller);
    }

    let mut queued = requirements
        .iter()
        .map(|requirement| !requirement.is_empty())
        .collect::<Vec<_>>();
    let mut pending = queued
        .iter()
        .enumerate()
        .filter_map(|(idx, queued)| queued.then_some(idx))
        .collect::<VecDeque<_>>();

    while let Some(callee) = pending.pop_front() {
        queued[callee] = false;
        let propagated = requirements[callee].clone();
        for &caller in &callers_by_callee[callee] {
            let before = requirements[caller].len();
            requirements[caller].extend(propagated.iter().copied());
            if requirements[caller].len() > before && !queued[caller] {
                queued[caller] = true;
                pending.push_back(caller);
            }
        }
    }
}

fn desired_carrier(
    callable: &ProgramCallable,
    requirements: &BTreeSet<CapabilityId>,
) -> Option<CarrierKind> {
    if callable.flow_predicate
        && requirements.len() == 1
        && requirements.contains(&CapabilityId::Ast)
    {
        return Some(CarrierKind::Narrow(CapabilityId::Ast));
    }
    if matches!(
        callable.carrier.as_ref().map(|carrier| &carrier.kind),
        Some(CarrierKind::Root)
    ) && callable.root_attenuation.is_none()
    {
        return Some(CarrierKind::Root);
    }
    if requirements.is_empty() {
        return callable
            .carrier
            .as_ref()
            .map(|carrier| carrier.kind.clone());
    }
    if callable.boundary {
        return Some(CarrierKind::Root);
    }
    if requirements.len() == 1 {
        return Some(CarrierKind::Narrow(
            *requirements.first().expect("one requirement"),
        ));
    }
    Some(CarrierKind::Bundle(requirements.clone()))
}

fn carrier_changed(current: Option<&Carrier>, desired: Option<&CarrierKind>) -> bool {
    match (current, desired) {
        (Some(current), Some(desired)) => current.kind != *desired,
        (None, Some(_)) => true,
        _ => false,
    }
}

fn signature_edit(
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

fn receiver_projection_edits(callable: &ProgramCallable, desired: &CarrierKind) -> Vec<FixEdit> {
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

fn ambient_edits(
    source: &str,
    callable: &ProgramCallable,
    desired: &CarrierKind,
    diagnostics: Option<&FileDiagnostics<'_>>,
) -> Vec<FixEdit> {
    let Some(diagnostics) = diagnostics else {
        return Vec::new();
    };
    let binding = final_binding(callable).unwrap_or("harness");
    let mut edits = Vec::new();
    for ambient in &callable.info.ambient_capability_calls {
        if !diagnostics.ambient_spans.contains(&(
            ambient.code,
            ambient.span.start,
            ambient.span.end,
        )) {
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

fn explicit_capability_argument_edits(
    source: &str,
    callable: &ProgramCallable,
    desired: &CarrierKind,
    diagnostics: Option<&FileDiagnostics<'_>>,
) -> Vec<FixEdit> {
    let Some(diagnostics) = diagnostics else {
        return Vec::new();
    };
    let Some(binding) = final_binding(callable) else {
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

fn is_missing_capability_argument(diagnostic: &RepairCandidate) -> bool {
    diagnostic.code == Code::ArgumentTypeMismatch
        && matches!(
            diagnostic.repair.id.as_str(),
            "bindings/prepend-capability-argument" | "bindings/thread-root-argument"
        )
}

fn diagnostic_capability(diagnostic: &RepairCandidate) -> Option<CapabilityId> {
    let TypeExpr::Named(expected) = diagnostic.expected_type.as_ref()? else {
        return None;
    };
    CapabilityId::from_type_name(expected)
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

fn add_call_argument_at_index_edit(
    source: &str,
    call: &super::CallSite,
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

fn argument_projection(
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

fn diagnostic_index(diagnostics: &[RepairCandidate]) -> BTreeMap<PathBuf, FileDiagnostics<'_>> {
    diagnostic_index_with(diagnostics, canonical)
}

fn diagnostic_index_with<'a>(
    diagnostics: &'a [RepairCandidate],
    mut normalize_path: impl FnMut(&Path) -> PathBuf,
) -> BTreeMap<PathBuf, FileDiagnostics<'a>> {
    let mut by_file = BTreeMap::<PathBuf, FileDiagnostics<'a>>::new();
    for diagnostic in diagnostics {
        let ambient = is_ambient_code(diagnostic.code);
        let missing_capability = is_missing_capability_argument(diagnostic);
        if !ambient && !missing_capability {
            continue;
        }
        let entry = by_file
            .entry(normalize_path(Path::new(&diagnostic.file)))
            .or_default();
        if ambient {
            entry.representative_ambient_code = Some(diagnostic.code);
            if let Some(span) = diagnostic.span {
                entry
                    .ambient_spans
                    .insert((diagnostic.code, span.start, span.end));
            }
        }
        if missing_capability {
            entry.missing_capability_arguments.push(diagnostic);
        }
    }
    by_file
}

fn is_ambient_code(code: Code) -> bool {
    ambient_capability_handle(code).is_some()
}

fn dedupe(edits: Vec<FixEdit>) -> Vec<FixEdit> {
    let mut by_key = BTreeMap::new();
    for edit in edits {
        by_key.insert(
            (edit.span.start, edit.span.end, edit.replacement.clone()),
            edit,
        );
    }
    by_key.into_values().collect()
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn diagnostic(
        file: &str,
        code: Code,
        repair_id: &str,
        expected_type: Option<TypeExpr>,
    ) -> RepairCandidate {
        RepairCandidate {
            file: file.to_string(),
            source: "test",
            severity: "warning",
            code,
            message: "test diagnostic".to_string(),
            unresolved_name: None,
            expected_type,
            span: Some(Span::with_offsets(4, 8, 1, 5)),
            repair: Repair {
                id: harn_parser::RepairId::from_owned(repair_id.to_string()),
                summary: "test repair".to_string(),
                safety: RepairSafety::ScopeLocal,
            },
            impact: RepairImpactWire::generic(),
            edits: Vec::new(),
        }
    }

    #[test]
    fn diagnostic_index_normalizes_each_relevant_path_once() {
        let diagnostics = vec![
            diagnostic(
                "a.harn",
                Code::LintAmbientFsBuiltin,
                "bindings/thread-harness-fs",
                None,
            ),
            diagnostic(
                "a.harn",
                Code::ArgumentTypeMismatch,
                "bindings/prepend-capability-argument",
                Some(TypeExpr::Named("HarnessFs".to_string())),
            ),
            diagnostic(
                "b.harn",
                Code::FormatterWouldReformat,
                "format/reformat",
                None,
            ),
        ];
        let normalizations = Cell::new(0);

        let index = diagnostic_index_with(&diagnostics, |path| {
            normalizations.set(normalizations.get() + 1);
            path.to_path_buf()
        });

        assert_eq!(normalizations.get(), 2);
        let indexed = index.get(Path::new("a.harn")).unwrap();
        assert_eq!(indexed.ambient_spans.len(), 1);
        assert_eq!(indexed.missing_capability_arguments.len(), 1);
        assert!(!index.contains_key(Path::new("b.harn")));
    }
}
