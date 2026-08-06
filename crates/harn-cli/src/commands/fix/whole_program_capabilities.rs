//! Plan capability-carrier signatures and calls for the complete invocation.
//!
//! The per-file linter remains the diagnostic owner. This module turns those
//! diagnostics into one program-wide edit graph so a narrowed signature and
//! every reachable caller move in the same apply pass.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};

use harn_builtin_meta::CapabilityId;
use harn_lexer::{FixEdit, Span};
use harn_parser::{
    visit, DiagnosticCode as Code, Node, Repair, RepairSafety, SNode, TypeExpr, TypedParam,
};

use super::capability_migrations::ambient_capability_handle;
use super::signature_threading::{
    add_call_argument_edit, collect_callable_infos, collect_value_reference_sites,
};
use super::value_escape::{FrozenCallable, FrozenCause};
use super::value_wrap::{wrap_value_reference_edit, ValueReferenceSite};
use super::{CallableInfo, RepairCandidate, RepairImpactWire, SignatureChangeWire};

#[path = "whole_program_capabilities/edits.rs"]
mod edits;
#[path = "whole_program_capabilities/imported_calls.rs"]
mod imported_calls;

use edits::{
    add_call_argument_at_index_edit, add_call_arguments_at_index_edit, ambient_edits,
    argument_for_kind, carrier_supplies, explicit_capability_argument_edits,
    receiver_projection_edits, signature_edit, split_call_extension,
    split_capability_receiver_edits, split_capability_signature_edit, undefined_harness_edits,
};
use imported_calls::{
    argument_edits as imported_argument_edits, signatures as imported_signatures,
};

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
    imported_capability_signatures: BTreeMap<String, imported_calls::Signature>,
    value_reference_sites: Vec<ValueReferenceSite>,
}

#[derive(Debug)]
struct ProgramCallable {
    file_idx: usize,
    info: CallableInfo,
    receiver_accesses: Vec<ReceiverAccess>,
    direct_receiver_spans: Vec<Span>,
    undefined_harness_accesses: Vec<ReceiverAccess>,
    boundary: bool,
    flow_predicate: bool,
    carrier: Option<Carrier>,
    carriers: Vec<Carrier>,
    imported_binding_evidence:
        BTreeMap<harn_parser::lexical::BindingId, imported_calls::BindingEvidence>,
    resolved_imported_bindings: HashMap<(usize, usize), harn_parser::lexical::BindingId>,
    has_split_capability_params: bool,
    root_attenuation: Option<BTreeSet<CapabilityId>>,
    direct_requirements: BTreeSet<CapabilityId>,
    direct_root_requirement: bool,
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
    referenced_by_value: &BTreeSet<String>,
    manifest_host_entries: &super::manifest_host_entries::ManifestHostEntries,
    frozen: &mut Vec<FrozenCallable>,
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
        let root_attenuations = harn_lint::capability_attenuations(
            &source,
            &program,
            crate::package::is_declared_connector_module(file),
        )
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
        let type_aliases = capability_type_aliases(&program, file, module_graph);
        let type_facts = crate::commands::check::typecheck_config(
            file,
            &crate::package::CheckConfig::default(),
            module_graph,
        )
        .check_with_facts(&program, &source);
        let boundaries = harn_lint::RuntimeBoundaries::collect(
            &program,
            crate::package::is_declared_connector_module(file),
        );
        let infos = collect_callable_infos(
            &program,
            &source,
            &exported,
            referenced_by_value,
            manifest_host_entries.names_for(file),
        );
        let imported_capability_signatures = imported_signatures(file, module_graph, &type_aliases);
        for info in infos {
            let Some((params, body, boundary, flow_predicate)) =
                declaration_parts(&program, &boundaries, info.span)
            else {
                continue;
            };
            let carriers = capability_carriers(params, &type_aliases);
            let imported_binding_evidence = imported_calls::binding_evidence(
                params,
                body,
                &type_aliases,
                &type_facts.binding_types,
            );
            let resolved_imported_bindings =
                harn_parser::lexical::resolved_identifier_bindings(params, body);
            let has_split_capability_params = carriers.len() > 1
                || matches!(
                    carriers.first().map(|carrier| &carrier.kind),
                    Some(CarrierKind::Narrow(_))
                );
            let carrier = carriers.first().cloned();
            let root_attenuation = root_attenuations
                .get(&(info.span.start, info.span.end))
                .cloned();
            let mut direct_requirements = direct_requirements(
                &source,
                params,
                body,
                &carriers,
                carrier.as_ref(),
                root_attenuation.as_ref(),
            );
            let mut direct_root_requirement = false;
            for call in &info.calls {
                let Some(signature) = imported_capability_signatures.get(&call.callee) else {
                    continue;
                };
                let Some(repair) = imported_calls::prefix_repair(
                    &source,
                    &carriers,
                    &imported_binding_evidence,
                    &resolved_imported_bindings,
                    call,
                    signature,
                ) else {
                    continue;
                };
                for kind in repair.missing_kinds(signature) {
                    match kind {
                        CarrierKind::Root => direct_root_requirement = true,
                        CarrierKind::Narrow(capability) => {
                            direct_requirements.insert(*capability);
                        }
                        CarrierKind::Bundle(capabilities) => {
                            direct_requirements.extend(capabilities.iter().copied());
                        }
                    }
                }
            }
            let receiver = carrier
                .as_ref()
                .map_or("harness", |carrier| carrier.name.as_str());
            let receiver_accesses = collect_receiver_accesses(&source, body, receiver);
            let direct_receiver_spans = collect_direct_receiver_spans(body, receiver);
            let undefined_harness_accesses =
                if receiver != "harness" && !info.bound_names.contains("harness") {
                    collect_receiver_accesses(&source, body, "harness")
                } else {
                    Vec::new()
                };
            callables.push(ProgramCallable {
                file_idx,
                info,
                receiver_accesses,
                direct_receiver_spans,
                undefined_harness_accesses,
                boundary,
                flow_predicate,
                carrier,
                carriers,
                imported_binding_evidence,
                resolved_imported_bindings,
                has_split_capability_params,
                root_attenuation,
                direct_requirements,
                direct_root_requirement,
            });
        }
        let value_reference_sites = collect_value_reference_sites(&program);
        program_files.push(ProgramFile {
            path: canonical(file),
            source,
            imported_capability_signatures,
            value_reference_sites,
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
    let mut root_requirements = callables
        .iter()
        .map(|callable| callable.direct_root_requirement)
        .collect::<Vec<_>>();
    propagate_carrier_requirements(
        &edges,
        &callables,
        &mut requirements,
        &mut root_requirements,
    );

    for ((callable, required), root_required) in
        callables.iter().zip(&requirements).zip(&root_requirements)
    {
        if !callable.flow_predicate {
            continue;
        }
        let unsupported = required
            .iter()
            .filter(|capability| **capability != CapabilityId::Ast)
            .map(|capability| capability.field_name())
            .collect::<Vec<_>>();
        if *root_required {
            return Err(format!(
                "flow predicate `{}` requires unsupported root Harness authority; flow evaluation injects only HarnessAst",
                callable.info.name
            ));
        }
        if !unsupported.is_empty() {
            return Err(format!(
                "flow predicate `{}` requires unsupported injected capabilities: {}; flow evaluation injects only HarnessAst",
                callable.info.name,
                unsupported.join(", ")
            ));
        }
    }

    // A callable whose value is taken as a first-class reference is invoked
    // through that reference at its declared arity: `handler: web_search_handler`
    // is dispatched as `handler(args)`. Adding a leading capability parameter
    // without repairing the hand-over would silently move `args` into the
    // capability slot. Prefer synthesizing the wrap the decline message already
    // describes; freeze only when a site cannot receive `harness`.
    //
    // A value reference is one way a caller becomes invisible; a declared entry
    // point is the other. `@host_entry` and a `harn.toml` handler both fix the
    // arity from outside this program (#6193 / #6272) and cannot be wrapped —
    // those stay frozen via `frozen_cause`.
    let arity_observable = callables
        .iter()
        .map(|callable| {
            referenced_by_value.contains(&callable.info.name)
                || callable.info.frozen_cause.is_some()
        })
        .collect::<Vec<_>>();
    let wrappable = mark_value_reference_wraps(
        &program_files,
        &callables,
        &arity_observable,
        &requirements,
        &mut root_requirements,
        frozen,
    );
    // Wrapping a hand-over site may newly require root Harness in the
    // containing callable; re-propagate so callers of those containers move too.
    propagate_carrier_requirements(
        &edges,
        &callables,
        &mut requirements,
        &mut root_requirements,
    );
    let desired = callables
        .iter()
        .enumerate()
        .map(|(idx, callable)| {
            if arity_observable[idx] && !wrappable[idx] {
                return callable
                    .carrier
                    .as_ref()
                    .map(|carrier| carrier.kind.clone());
            }
            desired_carrier(callable, &requirements[idx], root_requirements[idx])
        })
        .collect::<Vec<_>>();
    let added_capabilities = callables
        .iter()
        .zip(&requirements)
        .enumerate()
        .map(|(idx, (callable, required))| {
            if arity_observable[idx] && !wrappable[idx] {
                return BTreeMap::new();
            }
            added_split_capability_bindings(callable, required, root_requirements[idx])
        })
        .collect::<Vec<_>>();
    // Emit wraps only after desired carriers are known, so the inner call's
    // leading argument matches what signature threading would pass.
    let wrap_edits_by_file = emit_value_reference_wraps(
        &program_files,
        &callables,
        &wrappable,
        &desired,
        &added_capabilities,
    )?;
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
    let has_missing_imported_capability_arguments = callables.iter().any(|callable| {
        let file = &program_files[callable.file_idx];
        callable.info.calls.iter().any(|call| {
            file.imported_capability_signatures
                .get(&call.callee)
                .is_some_and(|signature| {
                    imported_calls::prefix_repair(
                        &file.source,
                        &callable.carriers,
                        &callable.imported_binding_evidence,
                        &callable.resolved_imported_bindings,
                        call,
                        signature,
                    )
                    .is_some()
                })
        })
    });
    if !signature_changed.iter().any(|changed| *changed)
        && !callables
            .iter()
            .any(|callable| !callable.info.ambient_capability_calls.is_empty())
        && !has_missing_imported_capability_arguments
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
    for (file_idx, edits) in wrap_edits_by_file {
        edits_by_file.entry(file_idx).or_default().extend(edits);
    }
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
            edits_by_file.entry(callable.file_idx).or_default().extend(
                split_capability_receiver_edits(callable, &added_capabilities[idx]),
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
        edits_by_file
            .entry(callable.file_idx)
            .or_default()
            .extend(undefined_harness_edits(
                callable,
                desired,
                &added_capabilities[idx],
            ));
        edits_by_file.entry(callable.file_idx).or_default().extend(
            explicit_capability_argument_edits(
                &program_files[callable.file_idx].source,
                callable,
                desired,
                &added_capabilities[idx],
                diagnostics_by_file.get(&program_files[callable.file_idx].path),
                &program_files[callable.file_idx].imported_capability_signatures,
            ),
        );
        edits_by_file
            .entry(callable.file_idx)
            .or_default()
            .extend(imported_argument_edits(
                &program_files[callable.file_idx],
                callable,
                desired,
                &added_capabilities[idx],
            ));
    }
    for edge in &edges {
        if !signature_changed[edge.callee] && !signature_changed[edge.caller] {
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
            )
            .map_err(|error| {
                call_edge_error(
                    &program_files[caller.file_idx].path,
                    &caller.info.name,
                    &callee.info.name,
                    call.span,
                    &error,
                )
            })?;
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
        } else if signature_changed[edge.caller] {
            if let Some(carrier) = &callee.carrier {
                if let Some(argument_span) = call.args.get(carrier.param_index).copied() {
                    let source = &program_files[caller.file_idx].source;
                    let existing_argument = source
                        .get(argument_span.start..argument_span.end)
                        .map(str::trim);
                    let projects_caller_carrier =
                        caller.carrier.as_ref().is_some_and(|caller_carrier| {
                            existing_argument == Some(caller_carrier.name.as_str())
                        });
                    if projects_caller_carrier {
                        let argument = argument_for_kind(
                            caller,
                            caller_desired,
                            &added_capabilities[edge.caller],
                            &carrier.kind,
                        )
                        .map_err(|error| {
                            call_edge_error(
                                &program_files[caller.file_idx].path,
                                &caller.info.name,
                                &callee.info.name,
                                call.span,
                                &error,
                            )
                        })?;
                        edits_by_file
                            .entry(caller.file_idx)
                            .or_default()
                            .push(FixEdit {
                                span: argument_span,
                                replacement: argument,
                            });
                    }
                }
            }
        }
        if !added_capabilities[edge.callee].is_empty() {
            let Some((index, arguments)) = split_call_extension(
                caller,
                caller_desired,
                &added_capabilities[edge.caller],
                callee,
                &added_capabilities[edge.callee],
                call.args.len(),
            )
            .map_err(|error| {
                call_edge_error(
                    &program_files[caller.file_idx].path,
                    &caller.info.name,
                    &callee.info.name,
                    call.span,
                    &error,
                )
            })?
            else {
                // The call omits a non-capability positional argument. The
                // fixer cannot synthesize that value, so leave it untouched.
                continue;
            };
            let edit = add_call_arguments_at_index_edit(
                &program_files[caller.file_idx].source,
                call,
                index,
                &arguments,
            )
            .expect("split extension index is bounded by the observed call arity");
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

/// Decide which value-referenced callables can be unblocked by a wrap.
///
/// Containers that must supply the capability into a wrap get
/// `root_requirements` set so the subsequent propagate/desired pass threads
/// them. Wrap text is emitted later in [`emit_value_reference_wraps`] once
/// desired carriers are known.
fn mark_value_reference_wraps(
    program_files: &[ProgramFile],
    callables: &[ProgramCallable],
    arity_observable: &[bool],
    requirements: &[BTreeSet<CapabilityId>],
    root_requirements: &mut [bool],
    frozen: &mut Vec<FrozenCallable>,
) -> Vec<bool> {
    let mut wrappable = vec![false; callables.len()];
    let callable_by_name = callables
        .iter()
        .enumerate()
        .map(|(idx, callable)| (callable.info.name.as_str(), idx))
        .collect::<BTreeMap<_, _>>();
    let mut sites_by_name: BTreeMap<&str, Vec<(usize, Span)>> = BTreeMap::new();
    for (file_idx, file) in program_files.iter().enumerate() {
        for site in &file.value_reference_sites {
            if callable_by_name.contains_key(site.name.as_str()) {
                sites_by_name
                    .entry(site.name.as_str())
                    .or_default()
                    .push((file_idx, site.span));
            }
        }
    }

    for (idx, callable) in callables.iter().enumerate() {
        if !arity_observable[idx] {
            continue;
        }
        let needs_new_param = callable.carrier.is_none()
            && (!requirements[idx].is_empty()
                || root_requirements[idx]
                || !callable.info.ambient_capability_calls.is_empty());
        if !needs_new_param {
            continue;
        }

        let sites = sites_by_name
            .get(callable.info.name.as_str())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let site_locations = || {
            sites
                .iter()
                .map(|(file_idx, span)| {
                    (
                        program_files[*file_idx].path.display().to_string(),
                        span.line,
                    )
                })
                .collect::<Vec<_>>()
        };

        // Host and manifest entries fix arity outside this program; wraps
        // cannot invent an argument the embedding runtime was never asked to
        // supply (#6193 / #6272).
        if let Some(cause @ (FrozenCause::HostEntry | FrozenCause::ManifestHandler)) =
            callable.info.frozen_cause
        {
            record_frozen_callable(frozen, &callable.info.name, cause, &site_locations());
            continue;
        }
        if sites.is_empty() {
            record_frozen_callable(
                frozen,
                &callable.info.name,
                FrozenCause::ValueReference,
                &[],
            );
            continue;
        }

        let mut container_idxs = Vec::new();
        let mut refused = false;
        for &(file_idx, span) in sites {
            let Some(container_idx) = containing_callable(callables, file_idx, span) else {
                refused = true;
                break;
            };
            if matches!(
                callables[container_idx].info.frozen_cause,
                Some(FrozenCause::HostEntry | FrozenCause::ManifestHandler)
            ) || callables[container_idx].info.is_host_entry
            {
                refused = true;
                break;
            }
            if wrap_value_reference_edit(
                &program_files[file_idx].source,
                span,
                &callable.info.name,
                &callable.info.param_names,
            )
            .is_none()
            {
                refused = true;
                break;
            }
            container_idxs.push(container_idx);
        }
        if refused {
            record_frozen_callable(
                frozen,
                &callable.info.name,
                FrozenCause::ValueReference,
                &site_locations(),
            );
            continue;
        }

        for container_idx in container_idxs {
            root_requirements[container_idx] = true;
        }
        wrappable[idx] = true;
    }

    wrappable
}

fn emit_value_reference_wraps(
    program_files: &[ProgramFile],
    callables: &[ProgramCallable],
    wrappable: &[bool],
    desired: &[Option<CarrierKind>],
    added_capabilities: &[BTreeMap<CapabilityId, String>],
) -> Result<BTreeMap<usize, Vec<FixEdit>>, String> {
    let mut edits_by_file: BTreeMap<usize, Vec<FixEdit>> = BTreeMap::new();
    let callable_by_name = callables
        .iter()
        .enumerate()
        .map(|(idx, callable)| (callable.info.name.as_str(), idx))
        .collect::<BTreeMap<_, _>>();

    for (file_idx, file) in program_files.iter().enumerate() {
        for site in &file.value_reference_sites {
            let Some(&callee_idx) = callable_by_name.get(site.name.as_str()) else {
                continue;
            };
            if !wrappable[callee_idx] {
                continue;
            }
            let Some(container_idx) = containing_callable(callables, file_idx, site.span) else {
                continue;
            };
            let callee = &callables[callee_idx];
            let caller = &callables[container_idx];
            let Some(caller_desired) = desired[container_idx].as_ref() else {
                return Err(format!(
                    "wrap for `{}` needs a capability in `{}`, but none was planned",
                    callee.info.name, caller.info.name
                ));
            };
            let Some(callee_desired) = desired[callee_idx].as_ref() else {
                return Err(format!(
                    "wrap for `{}` planned without a desired carrier",
                    callee.info.name
                ));
            };
            let argument = argument_for_kind(
                caller,
                caller_desired,
                &added_capabilities[container_idx],
                callee_desired,
            )
            .map_err(|error| {
                format!(
                    "wrap for `{}` inside `{}`: {error}",
                    callee.info.name, caller.info.name
                )
            })?;
            let params = callee.info.param_names.join(", ");
            let call_args = if params.is_empty() {
                argument
            } else {
                format!("{argument}, {params}")
            };
            let replacement = if params.is_empty() {
                format!("{{ -> {}({call_args}) }}", callee.info.name)
            } else {
                format!("{{ {params} -> {}({call_args}) }}", callee.info.name)
            };
            let region = file
                .source
                .get(site.span.start..site.span.end)
                .ok_or_else(|| {
                    format!(
                        "wrap site for `{}` is out of range in {}",
                        callee.info.name,
                        file.path.display()
                    )
                })?;
            if region != callee.info.name {
                return Err(format!(
                    "wrap site for `{}` in {} no longer names the callable",
                    callee.info.name,
                    file.path.display()
                ));
            }
            edits_by_file.entry(file_idx).or_default().push(FixEdit {
                span: site.span,
                replacement,
            });
        }
    }
    Ok(edits_by_file)
}

fn containing_callable(
    callables: &[ProgramCallable],
    file_idx: usize,
    span: Span,
) -> Option<usize> {
    callables
        .iter()
        .enumerate()
        .filter(|(_, callable)| {
            callable.file_idx == file_idx
                && callable.info.span.start <= span.start
                && callable.info.span.end >= span.end
        })
        .min_by_key(|(_, callable)| {
            callable
                .info
                .span
                .end
                .saturating_sub(callable.info.span.start)
        })
        .map(|(idx, _)| idx)
}

fn record_frozen_callable(
    frozen: &mut Vec<FrozenCallable>,
    name: &str,
    cause: FrozenCause,
    sites: &[(String, usize)],
) {
    if let Some(existing) = frozen.iter_mut().find(|entry| entry.name == name) {
        // Per-file synthesis may have recorded the freeze without sites; prefer
        // the whole-program reason that names the hand-over locations.
        if !sites.is_empty() && !existing.reason.contains("escaping reference") {
            *existing = FrozenCallable::new(name, cause, sites);
        }
        return;
    }
    frozen.push(FrozenCallable::new(name, cause, sites));
}

fn declaration_parts<'a>(
    program: &'a [SNode],
    boundaries: &harn_lint::RuntimeBoundaries,
    span: Span,
) -> Option<(&'a [TypedParam], &'a [SNode], bool, bool)> {
    for node in program {
        let (attributes, inner) = harn_parser::peel_attributes(node);
        if inner.span.start != span.start || inner.span.end != span.end {
            continue;
        }
        let flow_predicate = harn_parser::is_flow_predicate_declaration(attributes, inner);
        let attributed = harn_lint::root_harness_boundary_attribute(node);
        return match &inner.node {
            Node::FnDecl {
                name,
                params,
                body,
                is_pub,
                ..
            } => Some((
                params,
                body,
                // `name == "main"` was the whole boundary test here, so a
                // connector runtime export — whose root `Harness` first
                // parameter the connector ABI pins — was attenuated like an
                // ordinary helper and the repaired package failed to load
                // (#6149). Ask the lint, which already owns this policy, so
                // the fixer cannot narrow a signature the diagnostic exempts.
                boundaries.contains(name, params, *is_pub, attributed),
                flow_predicate,
            )),
            Node::ToolDecl {
                name, params, body, ..
            } => Some((
                params,
                body,
                boundaries.contains(name, params, false, attributed),
                flow_predicate,
            )),
            Node::Pipeline { params, body, .. } => Some((params, body, true, flow_predicate)),
            _ => None,
        };
    }
    None
}

fn collect_receiver_accesses(source: &str, body: &[SNode], receiver: &str) -> Vec<ReceiverAccess> {
    let mut accesses = Vec::new();
    visit::walk_program_interpolated(source, body, &mut |node| {
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

fn collect_direct_receiver_spans(body: &[SNode], receiver: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    visit::walk_program(body, &mut |node| {
        let Node::MethodCall { object, .. } = &node.node else {
            return;
        };
        if matches!(&object.node, Node::Identifier(name) if name == receiver) {
            spans.push(object.span);
        }
    });
    spans
}

fn capability_type_aliases(
    program: &[SNode],
    file: &Path,
    module_graph: &harn_modules::ModuleGraph,
) -> BTreeMap<String, TypeExpr> {
    let mut aliases = BTreeMap::new();
    if let Some(imported) = module_graph.imported_type_declarations_for_file(file) {
        collect_type_aliases(&imported, &mut aliases);
    }
    // The typechecker registers local declarations after imports, so a valid
    // local alias has the same precedence here as it does during checking.
    collect_type_aliases(program, &mut aliases);
    aliases
}

fn collect_type_aliases(nodes: &[SNode], aliases: &mut BTreeMap<String, TypeExpr>) {
    for node in nodes {
        let node = match &node.node {
            Node::AttributedDecl { inner, .. } => inner.as_ref(),
            _ => node,
        };
        let Node::TypeDecl {
            name,
            type_params,
            type_expr,
            ..
        } = &node.node
        else {
            continue;
        };
        if type_params.is_empty() {
            aliases.insert(name.clone(), type_expr.clone());
        }
    }
}

fn capability_carriers(
    params: &[TypedParam],
    type_aliases: &BTreeMap<String, TypeExpr>,
) -> Vec<Carrier> {
    params
        .iter()
        .enumerate()
        .filter_map(|(param_index, param)| {
            let kind = capability_carrier_kind(
                param.type_expr.as_ref()?,
                type_aliases,
                &mut BTreeSet::new(),
            )?;
            Some(Carrier {
                name: param.name.clone(),
                param_index,
                param: param.clone(),
                kind,
            })
        })
        .collect()
}

fn capability_carrier_kind(
    type_expr: &TypeExpr,
    type_aliases: &BTreeMap<String, TypeExpr>,
    resolving: &mut BTreeSet<String>,
) -> Option<CarrierKind> {
    match type_expr {
        TypeExpr::Named(name) if name == "Harness" => Some(CarrierKind::Root),
        TypeExpr::Named(name) => {
            if let Some(capability) = CapabilityId::from_type_name(name) {
                return Some(CarrierKind::Narrow(capability));
            }
            let alias = type_aliases.get(name)?;
            if !resolving.insert(name.clone()) {
                return None;
            }
            let kind = capability_carrier_kind(alias, type_aliases, resolving);
            resolving.remove(name);
            kind
        }
        TypeExpr::Shape(fields) => {
            let capabilities = fields
                .iter()
                .map(|field| {
                    match capability_carrier_kind(&field.type_expr, type_aliases, resolving)? {
                        CarrierKind::Narrow(capability) => Some(capability),
                        CarrierKind::Root | CarrierKind::Bundle(_) => None,
                    }
                })
                .collect::<Option<BTreeSet<_>>>()?;
            (!capabilities.is_empty()).then_some(CarrierKind::Bundle(capabilities))
        }
        _ => None,
    }
}

fn direct_requirements(
    source: &str,
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
    // Interpolation-aware: a capability used only inside `${...}` is still
    // used, and concluding otherwise deletes it from the signature while the
    // use remains. See visit::walk_program_interpolated.
    visit::walk_program_interpolated(source, body, &mut observe);
    required
}

fn seed_ambient_requirements(
    files: &[ProgramFile],
    callables: &mut [ProgramCallable],
    diagnostics_by_file: &BTreeMap<PathBuf, FileDiagnostics<'_>>,
) {
    for callable in callables.iter_mut() {
        let file_diagnostics = diagnostics_by_file.get(&files[callable.file_idx].path);
        let undefined = file_diagnostics.map(|diagnostics| &diagnostics.undefined_harness_spans);
        let accesses = if callable.carrier.is_none() {
            &mut callable.receiver_accesses
        } else {
            &mut callable.undefined_harness_accesses
        };
        // With no explicit carrier, diagnostics distinguish a genuinely
        // undefined ambient receiver from a local `harness` binding. Once a
        // differently named carrier exists, the legacy ambient bridge makes
        // stale `harness.*` accesses typecheck, so syntax is the only signal.
        if callable.carrier.is_none() {
            accesses.retain(|access| {
                undefined.is_some_and(|spans| {
                    spans.contains(&(access.object_span.start, access.object_span.end))
                })
            });
        }
        callable.direct_requirements.extend(
            accesses
                .iter()
                .filter_map(|access| CapabilityId::from_field_name(&access.property)),
        );
        // Retired `std/testing` wrappers are not ambient builtins, so nothing
        // above observes them. Their typed replacements still take explicit
        // handles, so seed that demand from the call itself.
        callable.direct_requirements.extend(
            callable
                .info
                .calls
                .iter()
                .flat_map(|call| super::retired_testing::retired_wrapper_capabilities(&call.callee))
                .copied(),
        );
        // A wrapper whose successor takes the whole `Harness` needs the root
        // itself, not a narrow bundle. Demanding only its constituent
        // capabilities lets the carrier collapse to `{llm, testing}`, which
        // satisfies the requirement but cannot be passed where a `Harness` is
        // expected — so the rewrite finds no argument and declines the file.
        let retired_wrapper_needs_root = callable
            .info
            .calls
            .iter()
            .any(|call| super::retired_testing::retired_wrapper_requires_root(&call.callee));
        callable.direct_root_requirement = retired_wrapper_needs_root
            || file_diagnostics.is_some_and(|diagnostics| {
                diagnostics
                    .missing_capability_arguments
                    .iter()
                    .any(|diagnostic| {
                        matches!(
                            diagnostic.expected_type.as_ref(),
                            Some(TypeExpr::Named(expected)) if expected == "Harness"
                        ) && diagnostic.span.is_some_and(|span| {
                            callable.info.span.start <= span.start
                                && callable.info.span.end >= span.end
                        })
                    })
            });
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
    }

    // Per-file synthesis returns `None` for a frozen owner, so its ambient
    // diagnostics never become plan candidates and the loop above never sees
    // them. The AST still recorded the ambient calls; seed from those so a
    // wrap can plan a carrier for the callee. Skip host/manifest entries: those
    // contracts must not gain a parameter the embedding runtime was never
    // asked to pass (#6193 / #6272).
    for callable in callables.iter_mut() {
        if callable.info.is_host_entry
            || matches!(
                callable.info.frozen_cause,
                Some(FrozenCause::HostEntry | FrozenCause::ManifestHandler)
            )
        {
            continue;
        }
        if !callable.direct_requirements.is_empty() || callable.direct_root_requirement {
            continue;
        }
        for call in &callable.info.ambient_capability_calls {
            if let Some(capability) = ambient_call_capability(call) {
                callable.direct_requirements.insert(capability);
            } else if call.code == Code::LintAmbientHarnessMethod {
                callable.direct_root_requirement = true;
            }
        }
    }

    for (path, diagnostics) in diagnostics_by_file {
        let Some(file_idx) = file_indices.get(path).copied() else {
            continue;
        };
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

fn propagate_requirements(
    edges: &[ProgramEdge],
    requirements: &mut [BTreeSet<CapabilityId>],
    root_requirements: &mut [bool],
) {
    let mut callers_by_callee = vec![BTreeSet::new(); requirements.len()];
    for edge in edges {
        callers_by_callee[edge.callee].insert(edge.caller);
    }

    let mut queued = requirements
        .iter()
        .zip(root_requirements.iter())
        .map(|(requirement, root_required)| !requirement.is_empty() || *root_required)
        .collect::<Vec<_>>();
    let mut pending = queued
        .iter()
        .enumerate()
        .filter_map(|(idx, queued)| queued.then_some(idx))
        .collect::<VecDeque<_>>();

    while let Some(callee) = pending.pop_front() {
        queued[callee] = false;
        let propagated = requirements[callee].clone();
        let root_propagated = root_requirements[callee];
        for &caller in &callers_by_callee[callee] {
            let before = requirements[caller].len();
            let root_before = root_requirements[caller];
            requirements[caller].extend(propagated.iter().copied());
            root_requirements[caller] |= root_propagated;
            if (requirements[caller].len() > before || root_requirements[caller] != root_before)
                && !queued[caller]
            {
                queued[caller] = true;
                pending.push_back(caller);
            }
        }
    }
}

/// Capability sets alone are not a complete call contract: a root Harness
/// selected for orchestration cannot be reconstructed from its child handles.
/// Feed that selected carrier back through the reverse call graph until both
/// capability requirements and root authority reach a fixed point.
fn propagate_carrier_requirements(
    edges: &[ProgramEdge],
    callables: &[ProgramCallable],
    requirements: &mut [BTreeSet<CapabilityId>],
    root_requirements: &mut [bool],
) {
    loop {
        propagate_requirements(edges, requirements, root_requirements);
        let mut discovered_root = false;
        for (idx, callable) in callables.iter().enumerate() {
            if !root_requirements[idx]
                && matches!(
                    desired_carrier(callable, &requirements[idx], false),
                    Some(CarrierKind::Root)
                )
            {
                root_requirements[idx] = true;
                discovered_root = true;
            }
        }
        if !discovered_root {
            return;
        }
    }
}

fn desired_carrier(
    callable: &ProgramCallable,
    requirements: &BTreeSet<CapabilityId>,
    root_required: bool,
) -> Option<CarrierKind> {
    if root_required {
        return Some(CarrierKind::Root);
    }
    if callable.has_split_capability_params {
        if split_carrier_becomes_root(callable, requirements, root_required) {
            return Some(CarrierKind::Root);
        }
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
    match requirements.len() {
        1 => Some(CarrierKind::Narrow(
            *requirements.first().expect("one requirement"),
        )),
        2 => Some(CarrierKind::Bundle(requirements.clone())),
        // HARN-LNT-069 defines one handle and a two-field record as the
        // attenuated helper shapes. Three or more capabilities are genuine
        // orchestration: retain or introduce the compact root Harness.
        _ => Some(CarrierKind::Root),
    }
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
    root_required: bool,
) -> BTreeMap<CapabilityId, String> {
    if !callable.has_split_capability_params
        || split_carrier_becomes_root(callable, requirements, root_required)
    {
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
                (2..=unavailable.len() + 2)
                    .map(|suffix| format!("{base}_{suffix}"))
                    .find(|candidate| !unavailable.contains(candidate))
                    .expect("bounded capability binding candidates contain a free name")
            });
        unavailable.insert(name.clone());
        additions.insert(*capability, name);
    }
    additions
}

fn split_carrier_becomes_root(
    callable: &ProgramCallable,
    requirements: &BTreeSet<CapabilityId>,
    root_required: bool,
) -> bool {
    // Widen the first carrier in place when root authority is required. Keep
    // every additional hand-authored capability parameter: collapsing or
    // deleting those parameters is outside this migration.
    root_required || (callable.carriers.len() == 1 && requirements.len() > 2)
}

pub(super) fn call_edge_error(
    file: &Path,
    caller: &str,
    callee: &str,
    span: Span,
    error: &str,
) -> String {
    format!(
        "cannot migrate call `{caller}` -> `{callee}` at {}:{}:{} (bytes {}..{}): {error}",
        file.display(),
        span.line,
        span.column,
        span.start,
        span.end
    )
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

    #[test]
    fn capability_carrier_alias_resolution_stops_at_cycles() {
        let aliases = BTreeMap::from([
            ("First".to_string(), TypeExpr::Named("Second".to_string())),
            ("Second".to_string(), TypeExpr::Named("First".to_string())),
        ]);

        assert_eq!(
            capability_carrier_kind(
                &TypeExpr::Named("First".to_string()),
                &aliases,
                &mut BTreeSet::new(),
            ),
            None
        );
    }
}
