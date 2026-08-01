//! Plan capability-carrier signatures and calls for the complete invocation.
//!
//! The per-file linter remains the diagnostic owner. This module turns those
//! diagnostics into one program-wide edit graph so a narrowed signature and
//! every reachable caller move in the same apply pass.

use std::collections::{BTreeMap, BTreeSet};
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
    params: Vec<TypedParam>,
    body: Vec<SNode>,
    boundary: bool,
    carrier: Option<Carrier>,
    direct_requirements: BTreeSet<CapabilityId>,
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
        let infos = collect_callable_infos(&program, &source, &exported);
        for info in infos {
            let Some((params, body, boundary)) = declaration_parts(&program, info.span) else {
                continue;
            };
            let carrier = capability_carrier(&params);
            let direct_requirements = direct_requirements(&params, &body, carrier.as_ref());
            callables.push(ProgramCallable {
                file_idx,
                info,
                params,
                body,
                boundary,
                carrier,
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
    {
        return Ok(Vec::new());
    }

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
                diagnostics,
                &program_files[callable.file_idx].path,
            ));
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
        let edit = if callee.carrier.is_some() {
            let first =
                call.args.first().copied().ok_or_else(|| {
                    format!("{} requires a capability argument", callee.info.name)
                })?;
            FixEdit {
                span: first,
                replacement: argument,
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

    let ambient_by_file = ambient_diagnostic_codes(diagnostics);
    let mut planned = Vec::new();
    for (file_idx, edits) in edits_by_file {
        let edits = dedupe(edits);
        if edits.is_empty() {
            continue;
        }
        let path = program_files[file_idx].path.to_string_lossy().into_owned();
        let code = ambient_by_file
            .get(&program_files[file_idx].path)
            .copied()
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
        planned.push(RepairCandidate {
            file: path,
            source: "whole-program",
            severity: "warning",
            code,
            message: "thread the least capability authority through the invocation graph"
                .to_string(),
            span: edits.first().map(|edit| edit.span),
            repair: Repair {
                id: harn_parser::RepairId::from_owned(
                    "bindings/thread-harness-whole-program".to_string(),
                ),
                summary: "Update capability signatures and all reachable call sites together"
                    .to_string(),
                safety: RepairSafety::SurfaceChanging,
            },
            impact: RepairImpactWire {
                classification: "whole-program-capability-change".to_string(),
                strategy: Some("whole-program-fixpoint".to_string()),
                signature_changes: signatures,
                requires_cross_module_caller_updates: true,
                notes: vec![
                    "requirements were propagated across resolved module imports before edits"
                        .to_string(),
                ],
            },
            edits,
        });
    }
    Ok(planned)
}

fn declaration_parts(program: &[SNode], span: Span) -> Option<(Vec<TypedParam>, Vec<SNode>, bool)> {
    for node in program {
        let inner = match &node.node {
            Node::AttributedDecl { inner, .. } => inner.as_ref(),
            _ => node,
        };
        if inner.span.start != span.start || inner.span.end != span.end {
            continue;
        }
        return match &inner.node {
            Node::FnDecl {
                name, params, body, ..
            }
            | Node::ToolDecl {
                name, params, body, ..
            } => Some((params.clone(), body.clone(), name == "main")),
            Node::Pipeline { params, body, .. } => Some((params.clone(), body.clone(), true)),
            _ => None,
        };
    }
    None
}

fn capability_carrier(params: &[TypedParam]) -> Option<Carrier> {
    params.iter().find_map(|param| {
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
            param: param.clone(),
            kind,
        })
    })
}

fn direct_requirements(
    params: &[TypedParam],
    body: &[SNode],
    carrier: Option<&Carrier>,
) -> BTreeSet<CapabilityId> {
    let Some(carrier) = carrier else {
        return BTreeSet::new();
    };
    let mut required = match &carrier.kind {
        CarrierKind::Narrow(capability) => BTreeSet::from([*capability]),
        CarrierKind::Bundle(capabilities) => capabilities.clone(),
        CarrierKind::Root => BTreeSet::new(),
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
        let imports = module_graph.imports_for_module(caller_path);
        for (call_idx, call) in caller.info.calls.iter().enumerate() {
            let local = by_file_name
                .get(&(caller_path.clone(), call.callee.clone()))
                .copied();
            let imported = imports.iter().filter_map(|import| {
                if import.namespace_alias.is_some()
                    || import
                        .selective_names
                        .as_ref()
                        .is_some_and(|names| !names.contains(&call.callee))
                {
                    return None;
                }
                let target = canonical(import.resolved_path.as_deref()?);
                by_file_name.get(&(target, call.callee.clone())).copied()
            });
            let mut targets = local.into_iter().chain(imported).collect::<BTreeSet<_>>();
            if targets.len() == 1 {
                edges.push(ProgramEdge {
                    caller: caller_idx,
                    call_idx,
                    callee: targets.pop_first().expect("one target"),
                });
            }
        }
    }
    edges
}

fn propagate_requirements(edges: &[ProgramEdge], requirements: &mut [BTreeSet<CapabilityId>]) {
    loop {
        let before = requirements.to_vec();
        for edge in edges {
            requirements[edge.caller].extend(before[edge.callee].iter().copied());
        }
        if *requirements == before {
            break;
        }
    }
}

fn desired_carrier(
    callable: &ProgramCallable,
    requirements: &BTreeSet<CapabilityId>,
) -> Option<CarrierKind> {
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
    let Some(carrier) = &callable.carrier else {
        return Vec::new();
    };
    let mut edits = Vec::new();
    match (&carrier.kind, desired) {
        (CarrierKind::Root, CarrierKind::Narrow(capability)) => {
            visit::walk_program(&callable.body, &mut |node| {
                let (Node::PropertyAccess { object, property }
                | Node::OptionalPropertyAccess { object, property }) = &node.node
                else {
                    return;
                };
                if property == capability.field_name()
                    && matches!(&object.node, Node::Identifier(name) if name == &carrier.name)
                {
                    edits.push(FixEdit {
                        span: node.span,
                        replacement: carrier.name.clone(),
                    });
                }
            });
        }
        (CarrierKind::Narrow(capability), CarrierKind::Bundle(_)) => {
            visit::walk_program(&callable.body, &mut |node| {
                let (Node::PropertyAccess { object, .. }
                | Node::OptionalPropertyAccess { object, .. }) = &node.node
                else {
                    return;
                };
                if matches!(&object.node, Node::Identifier(name) if name == &carrier.name) {
                    edits.push(FixEdit {
                        span: object.span,
                        replacement: format!("{}.{}", carrier.name, capability.field_name()),
                    });
                }
            });
        }
        _ => {}
    }
    edits
}

fn ambient_edits(
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

fn ambient_diagnostic_codes(diagnostics: &[RepairCandidate]) -> BTreeMap<PathBuf, Code> {
    diagnostics
        .iter()
        .filter(|diagnostic| is_ambient_code(diagnostic.code))
        .map(|diagnostic| (canonical(Path::new(&diagnostic.file)), diagnostic.code))
        .collect()
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
