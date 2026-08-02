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
    carriers: Vec<Carrier>,
    has_split_capability_params: bool,
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
    undefined_harness_spans: BTreeSet<(usize, usize)>,
}

struct CanonicalPathCache<F> {
    paths: BTreeMap<PathBuf, PathBuf>,
    normalize: F,
}

impl<F> CanonicalPathCache<F>
where
    F: FnMut(&Path) -> PathBuf,
{
    fn new(normalize: F) -> Self {
        Self {
            paths: BTreeMap::new(),
            normalize,
        }
    }

    fn get(&mut self, path: &Path) -> PathBuf {
        if let Some(normalized) = self.paths.get(path) {
            return normalized.clone();
        }
        let normalized = (self.normalize)(path);
        self.paths.insert(path.to_path_buf(), normalized.clone());
        normalized
    }
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
            let carriers = capability_carriers(params);
            let has_split_capability_params = carriers.len() > 1;
            let carrier = carriers.first().cloned();
            let root_attenuation = root_attenuations
                .get(&(info.span.start, info.span.end))
                .cloned();
            let direct_requirements = direct_requirements(
                params,
                body,
                &carriers,
                carrier.as_ref(),
                root_attenuation.as_ref(),
            );
            let receiver_accesses = collect_receiver_accesses(body, carrier.as_ref());
            callables.push(ProgramCallable {
                file_idx,
                info,
                receiver_accesses,
                boundary,
                flow_predicate,
                carrier,
                carriers,
                has_split_capability_params,
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

    let diagnostics_by_file = diagnostic_index(diagnostics);
    seed_ambient_requirements(&program_files, &mut callables, &diagnostics_by_file);
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
    let added_capabilities = callables
        .iter()
        .zip(&requirements)
        .map(|(callable, required)| added_split_capability_bindings(callable, required))
        .collect::<Vec<_>>();
    let changed = callables
        .iter()
        .zip(&desired)
        .map(|(callable, desired)| carrier_changed(callable.carrier.as_ref(), desired.as_ref()))
        .collect::<Vec<_>>();
    let signature_changed = changed
        .iter()
        .zip(&added_capabilities)
        .map(|(changed, added)| *changed || !added.is_empty())
        .collect::<Vec<_>>();
    if !signature_changed.iter().any(|changed| *changed)
        && !callables
            .iter()
            .any(|callable| !callable.info.ambient_capability_calls.is_empty())
        && !diagnostics.iter().any(is_missing_capability_argument)
    {
        return Ok(Vec::new());
    }
    let crosses_module_boundary = edges.iter().any(|edge| {
        signature_changed[edge.callee]
            && callables[edge.caller].file_idx != callables[edge.callee].file_idx
    });
    let surface_changing = crosses_module_boundary
        || callables.iter().enumerate().any(|(idx, callable)| {
            signature_changed[idx]
                && (callable.info.is_exported || (callable.boundary && callable.carrier.is_none()))
        });
    let repair_safety = if surface_changing {
        RepairSafety::SurfaceChanging
    } else {
        RepairSafety::ScopeLocal
    };

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
        if !added_capabilities[idx].is_empty() {
            edits_by_file.entry(callable.file_idx).or_default().push(
                split_capability_signature_edit(callable, &added_capabilities[idx])?,
            );
        }
        edits_by_file
            .entry(callable.file_idx)
            .or_default()
            .extend(ambient_edits(
                &program_files[callable.file_idx].source,
                callable,
                desired,
                &added_capabilities[idx],
                diagnostics_by_file.get(&program_files[callable.file_idx].path),
            ));
        edits_by_file.entry(callable.file_idx).or_default().extend(
            explicit_capability_argument_edits(
                &program_files[callable.file_idx].source,
                callable,
                desired,
                &added_capabilities[idx],
                diagnostics_by_file.get(&program_files[callable.file_idx].path),
            ),
        );
    }
    for edge in &edges {
        if !signature_changed[edge.callee] {
            continue;
        }
        let callee = &callables[edge.callee];
        let caller = &callables[edge.caller];
        let Some(caller_desired) = desired[edge.caller].as_ref() else {
            continue;
        };
        let call = &caller.info.calls[edge.call_idx];
        if changed[edge.callee] {
            let Some(callee_desired) = desired[edge.callee].as_ref() else {
                continue;
            };
            let argument = argument_for_kind(
                caller,
                caller_desired,
                &added_capabilities[edge.caller],
                callee_desired,
            )?;
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
        if !added_capabilities[edge.callee].is_empty() {
            let arguments = added_capabilities[edge.callee]
                .keys()
                .map(|capability| {
                    capability_value(
                        caller,
                        caller_desired,
                        &added_capabilities[edge.caller],
                        *capability,
                    )
                    .ok_or_else(|| {
                        format!(
                            "{} cannot supply {} to {}",
                            caller.info.name,
                            capability.type_name(),
                            callee.info.name
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let index = callee
                .carriers
                .iter()
                .map(|carrier| carrier.param_index)
                .max()
                .expect("split callable has capability carriers")
                + 1;
            let edit = add_call_arguments_at_index_edit(
                &program_files[caller.file_idx].source,
                call,
                index,
                &arguments,
            )
            .ok_or_else(|| format!("failed to extend call to {}", callee.info.name))?;
            edits_by_file.entry(caller.file_idx).or_default().push(edit);
        }
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
            .filter(|(idx, callable)| callable.file_idx == file_idx && signature_changed[*idx])
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

fn capability_carriers(params: &[TypedParam]) -> Vec<Carrier> {
    params
        .iter()
        .enumerate()
        .filter_map(|(param_index, param)| {
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
        .collect()
}

fn direct_requirements(
    params: &[TypedParam],
    body: &[SNode],
    carriers: &[Carrier],
    carrier: Option<&Carrier>,
    root_attenuation: Option<&BTreeSet<CapabilityId>>,
) -> BTreeSet<CapabilityId> {
    // Existing split parameters already satisfy their callable's authority.
    // Only diagnostics and callees may introduce a new requirement for a
    // split boundary; seeding every declared handle here would incorrectly
    // widen all of its callers.
    if carriers.len() > 1 {
        return BTreeSet::new();
    }
    let mut required = match carrier.map(|carrier| &carrier.kind) {
        Some(CarrierKind::Narrow(capability)) => BTreeSet::from([*capability]),
        Some(CarrierKind::Bundle(capabilities)) => capabilities.clone(),
        Some(CarrierKind::Root) | None => BTreeSet::new(),
    };
    let Some(carrier) = carrier else {
        return required;
    };
    if matches!(&carrier.kind, CarrierKind::Root)
        && root_attenuation.is_none()
        && required.is_empty()
    {
        return required;
    }
    if matches!(&carrier.kind, CarrierKind::Root) {
        required.extend(
            root_attenuation
                .iter()
                .flat_map(|capabilities| capabilities.iter().copied()),
        );
    }
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
    diagnostics_by_file: &BTreeMap<PathBuf, FileDiagnostics<'_>>,
) {
    for callable in callables
        .iter_mut()
        .filter(|callable| callable.carrier.is_none())
    {
        let undefined = diagnostics_by_file
            .get(&files[callable.file_idx].path)
            .map(|diagnostics| &diagnostics.undefined_harness_spans);
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

    let mut file_indices = BTreeMap::new();
    for (idx, file) in files.iter().enumerate() {
        file_indices.entry(file.path.clone()).or_insert(idx);
    }
    let mut callable_indices_by_file = vec![Vec::new(); files.len()];
    for (idx, callable) in callables.iter().enumerate() {
        callable_indices_by_file[callable.file_idx].push(idx);
    }
    for (path, diagnostics) in diagnostics_by_file {
        let Some(file_idx) = file_indices.get(path).copied() else {
            continue;
        };
        for (code, start, end) in &diagnostics.ambient_spans {
            for callable_idx in &callable_indices_by_file[file_idx] {
                let callable = &mut callables[*callable_idx];
                let Some(call) = callable.info.ambient_capability_calls.iter().find(|call| {
                    call.span.start == *start && call.span.end == *end && call.code == *code
                }) else {
                    continue;
                };
                let capability = ambient_call_capability(call);
                if let Some(capability) = capability {
                    callable.direct_requirements.insert(capability);
                }
            }
        }
        for diagnostic in &diagnostics.missing_capability_arguments {
            let Some(span) = diagnostic.span else {
                continue;
            };
            let Some(capability) = diagnostic_capability(diagnostic) else {
                continue;
            };
            let callable_idx = callable_indices_by_file[file_idx]
                .iter()
                .copied()
                .filter(|callable_idx| {
                    let callable = &callables[*callable_idx];
                    callable.info.span.start <= span.start && callable.info.span.end >= span.end
                })
                .min_by_key(|callable_idx| {
                    let callable = &callables[*callable_idx];
                    callable
                        .info
                        .span
                        .end
                        .saturating_sub(callable.info.span.start)
                });
            if let Some(callable_idx) = callable_idx {
                callables[callable_idx]
                    .direct_requirements
                    .insert(capability);
            }
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
    let mut canonical_paths = CanonicalPathCache::new(canonical);
    let mut edges = Vec::new();
    for (caller_idx, caller) in callables.iter().enumerate() {
        let caller_path = &files[caller.file_idx].path;
        for (call_idx, call) in caller.info.calls.iter().enumerate() {
            let target = module_graph
                .definition_of(caller_path, &call.callee)
                .and_then(|definition| {
                    by_file_name
                        .get(&(canonical_paths.get(&definition.file), definition.name))
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
    if callable.has_split_capability_params {
        return callable
            .carrier
            .as_ref()
            .map(|carrier| carrier.kind.clone());
    }
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

fn added_split_capability_bindings(
    callable: &ProgramCallable,
    requirements: &BTreeSet<CapabilityId>,
) -> BTreeMap<CapabilityId, String> {
    if !callable.has_split_capability_params {
        return BTreeMap::new();
    }
    let mut unavailable = callable.info.bound_names.clone();
    let mut additions = BTreeMap::new();
    for capability in requirements {
        if callable
            .carriers
            .iter()
            .any(|carrier| carrier_supplies(&carrier.kind, *capability))
        {
            continue;
        }
        let base = capability.field_name();
        let candidates = [
            base.to_string(),
            format!("_{base}"),
            format!("harness_{base}"),
        ];
        let name = candidates
            .into_iter()
            .find(|candidate| !unavailable.contains(candidate))
            .unwrap_or_else(|| {
                (2..)
                    .map(|suffix| format!("{base}_{suffix}"))
                    .find(|candidate| !unavailable.contains(candidate))
                    .expect("unbounded capability binding candidates")
            });
        unavailable.insert(name.clone());
        additions.insert(*capability, name);
    }
    additions
}

fn carrier_supplies(kind: &CarrierKind, capability: CapabilityId) -> bool {
    match kind {
        CarrierKind::Root => true,
        CarrierKind::Narrow(current) => *current == capability,
        CarrierKind::Bundle(capabilities) => capabilities.contains(&capability),
    }
}

fn split_capability_signature_edit(
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
        for carrier in &callable.carriers {
            if !carrier_supplies(&carrier.kind, capability) {
                continue;
            }
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

fn argument_for_kind(
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

fn explicit_capability_argument_edits(
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

fn ambient_call_capability(call: &super::AmbientCapabilityCall) -> Option<CapabilityId> {
    ambient_capability_handle(call.code)
        .filter(|field| !field.is_empty())
        .and_then(CapabilityId::from_field_name)
        .or_else(|| {
            harn_vm::stdlib::harness_migration_for_builtin(&call.name)
                .map(|migration| migration.capability)
        })
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

fn add_call_arguments_at_index_edit(
    source: &str,
    call: &super::CallSite,
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

fn diagnostic_index(diagnostics: &[RepairCandidate]) -> BTreeMap<PathBuf, FileDiagnostics<'_>> {
    diagnostic_index_with(diagnostics, canonical)
}

fn diagnostic_index_with<'a>(
    diagnostics: &'a [RepairCandidate],
    normalize_path: impl FnMut(&Path) -> PathBuf,
) -> BTreeMap<PathBuf, FileDiagnostics<'a>> {
    let mut by_file = BTreeMap::<PathBuf, FileDiagnostics<'a>>::new();
    let mut canonical_paths = CanonicalPathCache::new(normalize_path);
    for diagnostic in diagnostics {
        let ambient = is_ambient_code(diagnostic.code);
        let missing_capability = is_missing_capability_argument(diagnostic);
        let undefined_harness = diagnostic.code == Code::UndefinedVariable
            && diagnostic.unresolved_name.as_deref() == Some("harness");
        if !ambient && !missing_capability && !undefined_harness {
            continue;
        }
        let entry = by_file
            .entry(canonical_paths.get(Path::new(&diagnostic.file)))
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
        if undefined_harness {
            if let Some(span) = diagnostic.span {
                entry.undefined_harness_spans.insert((span.start, span.end));
            }
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
    fn diagnostic_index_normalizes_each_relevant_file_once() {
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
                Code::LintAmbientClockBuiltin,
                "bindings/thread-harness-clock",
                None,
            ),
            diagnostic(
                "c.harn",
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
        assert_eq!(index[Path::new("b.harn")].ambient_spans.len(), 1);
        assert!(!index.contains_key(Path::new("c.harn")));
    }
}
