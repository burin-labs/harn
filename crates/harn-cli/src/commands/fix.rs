//! `harn fix`: propose or apply repair-bearing diagnostics.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use crate::cli::FixArgs;
use crate::commands;
use crate::commands::check::collect_preflight_diagnostics_with_module_graph as preflight_diagnostics;
use crate::package::{self, CheckConfig, PreflightSeverity};
use harn_lexer::{FixEdit, Span};
use harn_lint::LintSeverity;
use harn_parser::analysis::{AnalysisDatabase, AnalysisError};
use harn_parser::{
    visit, DiagnosticCode as Code, DiagnosticDetails, DiagnosticSeverity, Node, Repair,
    RepairSafety, SNode, TypeExpr,
};

#[path = "fix/alias_widening.rs"]
mod alias_widening;
#[path = "fix/capability_arguments.rs"]
mod capability_arguments;
#[path = "fix/capability_migrations.rs"]
mod capability_migrations;
#[path = "fix/lint_context.rs"]
mod lint_context;
mod repair_classes;
#[path = "fix/reporting.rs"]
mod reporting;
use repair_classes::{
    defers_to_whole_program_pass, is_capability_migration_repair,
    is_whole_program_superseded_repair,
};
mod value_escape;
use value_escape::{FrozenCallable, FrozenCause, ValueEscape};
#[path = "fix/retired_testing.rs"]
mod retired_testing;
#[path = "fix/signature_threading.rs"]
mod signature_threading;
#[path = "fix/whole_program_capabilities.rs"]
mod whole_program_capabilities;
#[path = "fix/wire.rs"]
mod wire;
use capability_arguments::{
    capability_argument_for_span, insert_call_argument_before_span, root_harness_argument_for_span,
};
use capability_migrations::{ambient_call_rewrite, ambient_capability_handle, ambient_replacement};
use lint_context::FixLintContext;
#[path = "fix/apply.rs"]
mod apply;
use apply::apply_repairs_with_options;
#[cfg(test)]
use apply::{
    apply_capability_file_edits, apply_file_edits, apply_repairs, edit_group_key,
    finish_with_rollback, render_capability_migration_pass, repair_path,
};
use reporting::{print_apply_result, print_human_plan, skipped_files_error};
use signature_threading::{
    add_call_argument_edit, add_harness_param_edit, build_reverse_callers, collect_callable_infos,
    collect_value_references, harness_param_name_for_insert, propagate_harness_requirements,
    repair_for_ambient_capability_plan,
};
use wire::*;

pub(crate) const FIX_PLAN_SCHEMA_VERSION: u32 = 2;
pub(crate) const FIX_APPLY_SCHEMA_VERSION: u32 = 2;
const CAPABILITY_MIGRATION_MAX_PASSES: usize = 64;

#[derive(Debug, Clone)]
struct RepairCandidate {
    file: String,
    source: &'static str,
    severity: &'static str,
    code: Code,
    message: String,
    unresolved_name: Option<String>,
    expected_type: Option<TypeExpr>,
    span: Option<Span>,
    repair: Repair,
    impact: RepairImpactWire,
    edits: Vec<FixEdit>,
}

#[derive(Debug, Clone)]
struct CallableInfo {
    name: String,
    span: Span,
    is_exported: bool,
    insert_offset: usize,
    has_params: bool,
    bound_names: BTreeSet<String>,
    harness_binding: Option<String>,
    /// Set when something outside the fixer's view calls this at its declared
    /// arity, so a `harness` parameter must not be introduced. `None` is the
    /// ordinary case.
    frozen_cause: Option<FrozenCause>,
    /// The edits that must land with this callable's new parameter when its
    /// arity is fixed by a `type X = fn(...)` the migration may move: the alias
    /// declaration, and every call dispatched through a value it types.
    alias_widening_edits: Vec<FixEdit>,
    calls: Vec<CallSite>,
    ambient_capability_calls: Vec<AmbientCapabilityCall>,
}

#[derive(Debug, Clone)]
struct CallSite {
    callee: String,
    span: Span,
    args: Vec<Span>,
}

#[derive(Debug, Clone)]
struct AmbientCapabilityCall {
    name: String,
    code: Code,
    span: Span,
    args: Vec<Span>,
}

#[derive(Debug, Clone, Default)]
struct FixOptions {
    capability_migrations_only: bool,
    /// Diagnostic codes to plan and apply. Empty means every code, which is
    /// the behavior every caller had before the selector existed.
    codes: BTreeSet<Code>,
}

impl FixOptions {
    /// The capability-migration pass, which every migration test drives.
    /// A named constructor keeps a new option from touching each call site.
    #[cfg(test)]
    fn capability_migrations() -> Self {
        Self {
            capability_migrations_only: true,
            ..Self::default()
        }
    }

    /// Whether `--code` selected this candidate. An empty selector selects
    /// every code, which is the behavior every caller had before the flag.
    fn selects(&self, candidate: &RepairCandidate) -> bool {
        self.codes.is_empty() || self.codes.contains(&candidate.code)
    }
}

struct AmbientRepairContext {
    cross_module_importer_count: usize,
}

impl RepairImpactWire {
    fn generic() -> Self {
        Self {
            classification: "generic-repair".to_string(),
            strategy: None,
            signature_changes: Vec::new(),
            requires_cross_module_caller_updates: false,
            notes: Vec::new(),
        }
    }

    fn local_ambient(strategy: &'static str) -> Self {
        Self {
            classification: "local-ambient-rewrite".to_string(),
            strategy: Some(strategy.to_string()),
            signature_changes: Vec::new(),
            requires_cross_module_caller_updates: false,
            notes: Vec::new(),
        }
    }

    fn signature_threading(
        signature_changes: Vec<SignatureChangeWire>,
        cross_module_importer_count: usize,
    ) -> Self {
        let has_exported_change = signature_changes.iter().any(|change| change.is_exported);
        let has_public_change = signature_changes
            .iter()
            .any(|change| change.is_exported || change.is_entrypoint);
        let requires_cross_module_caller_updates =
            has_exported_change && cross_module_importer_count > 0;
        let mut notes = Vec::new();
        if requires_cross_module_caller_updates {
            notes.push(format!(
                "changes exported signatures in a file imported by {cross_module_importer_count} module(s); cross-module callers must be updated"
            ));
        } else if has_public_change {
            notes.push(
                "changes an exported or entrypoint signature; external callers may need updates"
                    .to_string(),
            );
        }

        Self {
            classification: if has_public_change {
                "public-signature-change"
            } else {
                "local-signature-threading"
            }
            .to_string(),
            strategy: Some("thread-params".to_string()),
            signature_changes,
            requires_cross_module_caller_updates,
            notes,
        }
    }
}

/// Resolve the target list the same way `harn check` does.
///
/// A capability migration propagates requirements across resolved module
/// imports, so a declaration and its cross-module callers have to be planned in
/// one pass or the callers are left stale. Before this accepted more than one
/// path, reaching two sibling trees meant naming their common ancestor — which
/// pulls in everything else under it, including deliberately-invalid parse
/// fixtures that then fail the whole run.
fn resolve_targets(args: &FixArgs) -> Result<Vec<PathBuf>, String> {
    let mut targets = args.paths.clone();
    if args.workspace {
        let anchor = targets.first().map(PathBuf::as_path);
        match package::load_workspace_config(anchor) {
            Some((workspace, manifest_dir)) if !workspace.pipelines.is_empty() => {
                for pipeline in workspace.pipelines {
                    let candidate = Path::new(&pipeline);
                    targets.push(if candidate.is_absolute() {
                        candidate.to_path_buf()
                    } else {
                        manifest_dir.join(candidate)
                    });
                }
            }
            Some(_) => {
                return Err(
                    "--workspace requires `[workspace].pipelines` in the nearest harn.toml"
                        .to_string(),
                );
            }
            None => {
                return Err(
                    "--workspace could not find a harn.toml walking up from the target(s)"
                        .to_string(),
                );
            }
        }
    }
    if targets.is_empty() {
        return Err(
            "`harn fix` requires at least one target path, or `--workspace` with `[workspace].pipelines`"
                .to_string(),
        );
    }
    Ok(targets)
}

pub(crate) fn run(args: &FixArgs) -> Result<(), FixRunError> {
    let targets = resolve_targets(args)?;
    if args.apply {
        let safety = args.safety.ok_or_else(|| {
            "`harn fix --apply` requires `--safety <format-only|behavior-preserving|scope-local|surface-changing|capability-changing>`"
                .to_string()
        })?;
        if safety == RepairSafety::NeedsHuman {
            return Err(
                "`harn fix --apply --safety needs-human` is not allowed; use `harn fix --plan --json` to inspect propose-only repairs"
                    .to_string()
                    .into(),
            );
        }
        let result = apply_repairs_with_options(
            &targets,
            safety,
            args.dry_run,
            FixOptions {
                capability_migrations_only: args.capability_migrations_only,
                codes: args.codes.iter().copied().collect(),
            },
        )?;
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&result)
                    .map_err(|error| format!("failed to serialize apply result: {error}"))?
            );
        } else {
            print_apply_result(&result);
        }
        if !result.skipped_files.is_empty() {
            return Err(FixRunError::PartialFailure(skipped_files_error(
                result.skipped_files.len(),
            )));
        }
        return Ok(());
    }
    if !args.plan {
        return Err("`harn fix` requires `--plan` or `--apply`"
            .to_string()
            .into());
    }

    let plan = build_plan_with_options(
        &targets,
        args.safety,
        &FixOptions {
            capability_migrations_only: args.capability_migrations_only,
            codes: args.codes.iter().copied().collect(),
        },
    )?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&plan)
                .map_err(|error| format!("failed to serialize repair plan: {error}"))?
        );
    } else {
        print_human_plan(&plan);
    }
    if !plan.skipped_files.is_empty() {
        return Err(FixRunError::PartialFailure(skipped_files_error(
            plan.skipped_files.len(),
        )));
    }
    Ok(())
}

/// Single-path wrappers for the test suite.
///
/// Production callers pass a target list because a capability migration has to
/// see a declaration and its cross-module callers in one pass. Almost every
/// test drives one temporary script, so wrapping the slice here keeps that
/// detail out of ~45 call sites.
#[cfg(test)]
fn build_plan_with_options_at(
    target: &Path,
    safety_ceiling: Option<RepairSafety>,
    options: &FixOptions,
) -> Result<RepairPlan, String> {
    build_plan_with_options(
        std::slice::from_ref(&target.to_path_buf()),
        safety_ceiling,
        options,
    )
}

#[cfg(test)]
fn apply_repairs_with_options_at(
    target: &Path,
    safety_ceiling: RepairSafety,
    dry_run: bool,
    options: FixOptions,
) -> Result<ApplyResult, String> {
    apply_repairs_with_options(
        std::slice::from_ref(&target.to_path_buf()),
        safety_ceiling,
        dry_run,
        options,
    )
}

#[cfg(test)]
pub(crate) fn build_plan(
    target: &Path,
    safety_ceiling: Option<RepairSafety>,
) -> Result<RepairPlan, String> {
    build_plan_with_options_at(target, safety_ceiling, &FixOptions::default())
}

fn build_plan_with_options(
    targets: &[PathBuf],
    safety_ceiling: Option<RepairSafety>,
    options: &FixOptions,
) -> Result<RepairPlan, String> {
    for target in targets {
        if let Err(error) = package::validate_runtime_manifest_extensions(target) {
            return Err(format!("manifest extension validation failed: {error}"));
        }
    }

    let target_strings: Vec<String> = targets
        .iter()
        .map(|target| target.to_string_lossy().into_owned())
        .collect();
    let target_refs: Vec<&str> = target_strings.iter().map(String::as_str).collect();
    let files = commands::check::collect_harn_targets(&target_refs);
    if files.is_empty() {
        return Err("no .harn files found under the given target(s)".to_string());
    }

    let module_graph = commands::check::build_module_graph(&files);
    let cross_file_imports = commands::check::collect_cross_file_imports(&module_graph);
    let mut analysis = AnalysisDatabase::new();
    // A callable whose value is read as a first-class reference is invoked at
    // its declared arity through a call site no per-file pass can see — a
    // registry entry in one module dispatching a handler defined in another.
    // Collect those names across the whole program before planning any file.
    let mut referenced_by_value = BTreeSet::new();
    for file in &files {
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };
        let Ok(program) = harn_parser::parse_source(&source) else {
            continue;
        };
        collect_value_references(&program, &mut referenced_by_value);
    }
    let referenced_by_value = &referenced_by_value;

    let mut candidates = Vec::new();
    let mut skipped_files = Vec::new();
    let mut frozen_callables: Vec<FrozenCallable> = Vec::new();
    for file in &files {
        if options.capability_migrations_only {
            if let Some(candidate) = retired_testing::repair(file) {
                candidates.push(candidate);
            }
        }
        if let Err(skipped) = collect_file_candidates(
            &mut analysis,
            file,
            safety_ceiling,
            &cross_file_imports,
            &module_graph,
            options,
            &mut candidates,
            &mut ValueEscape {
                referenced_by_value,
                frozen: &mut frozen_callables,
            },
        ) {
            skipped_files.push(skipped);
        }
    }

    let skipped_paths = skipped_files
        .iter()
        .map(|skipped| {
            std::fs::canonicalize(&skipped.path)
                .unwrap_or_else(|_| Path::new(&skipped.path).to_path_buf())
        })
        .collect::<BTreeSet<_>>();
    let valid_files = files
        .iter()
        .filter(|file| {
            !skipped_paths
                .contains(&std::fs::canonicalize(file).unwrap_or_else(|_| (*file).clone()))
        })
        .cloned()
        .collect::<Vec<_>>();
    let whole_program_repairs = whole_program_capabilities::plan(
        &valid_files,
        &module_graph,
        &candidates,
        referenced_by_value,
    )?;
    if !whole_program_repairs.is_empty() {
        // `--code` narrows what this pass *does*, not what it *saw*: the plan
        // needs every capability diagnostic as context to choose a carrier,
        // but a repair it emits for an unselected code is out of scope. That
        // distinction is also what keeps deferral from starving — deferring to
        // a pass that `--code` has excluded would postpone the local repair
        // forever, which is exactly what `--code HARN-LNT-073` did to every
        // rename in a file that also had attenuation work.
        let whole_program_repairs = whole_program_repairs
            .into_iter()
            .filter(|repair| options.selects(repair))
            .collect::<Vec<_>>();
        let whole_program_files = whole_program_repairs
            .iter()
            .map(|repair| {
                std::fs::canonicalize(&repair.file)
                    .unwrap_or_else(|_| Path::new(&repair.file).to_path_buf())
            })
            .collect::<BTreeSet<_>>();
        candidates.retain(|candidate| {
            // Supersession follows the real plan rather than the selected one:
            // a per-file repair the whole-program pass replaces is wrong on its
            // own terms, whether or not its replacement was selected.
            if is_whole_program_superseded_repair(&candidate.repair) {
                return false;
            }
            !defers_to_whole_program_pass(candidate.repair.id.as_str())
                || !whole_program_files.contains(
                    &std::fs::canonicalize(&candidate.file)
                        .unwrap_or_else(|_| Path::new(&candidate.file).to_path_buf()),
                )
        });
        candidates.extend(whole_program_repairs);
    }

    if options.capability_migrations_only {
        // This mode is consumed as an executable migration plan. A lint that
        // names a conceptual repair but has no source edit is context for the
        // whole-program planner, not a repair that apply can perform.
        candidates.retain(|candidate| !candidate.edits.is_empty());
    }

    // Narrowing here rather than at apply keeps the plan and the applied set
    // the same object: `--plan --code X` shows exactly what `--apply --code X`
    // will do, and `diagnostic_index` stays aligned because diagnostics and
    // repairs are both derived from `candidates` below.
    candidates.retain(|candidate| options.selects(candidate));

    let conflicts = detect_conflicts(&candidates);
    let diagnostics = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| DiagnosticWire {
            index,
            file: candidate.file.clone(),
            source: candidate.source,
            severity: candidate.severity,
            code: candidate.code.to_string(),
            message: candidate.message.clone(),
            span: candidate.span.map(SpanWire::from),
            repair: RepairMetadataWire::from(&candidate.repair),
        })
        .collect::<Vec<_>>();
    let repairs = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let conflicts_with = conflicts[index].clone();
            RepairWire {
                diagnostic_index: index,
                diagnostic_code: candidate.code.to_string(),
                repair: RepairMetadataWire::from(&candidate.repair),
                impact: candidate.impact.clone(),
                edits: candidate
                    .edits
                    .iter()
                    .map(FixEditWire::from)
                    .collect::<Vec<_>>(),
                applies_cleanly: conflicts_with.is_empty(),
                conflicts_with,
            }
        })
        .collect::<Vec<_>>();
    let present_safety = candidates
        .iter()
        .map(|candidate| candidate.repair.safety)
        .collect::<BTreeSet<_>>();
    let safety_levels = RepairSafety::ALL
        .iter()
        .copied()
        .filter(|safety| present_safety.contains(safety))
        .map(|safety| safety.as_str().to_string())
        .collect::<Vec<_>>();

    Ok(RepairPlan {
        schema_version: FIX_PLAN_SCHEMA_VERSION,
        path: target_strings.join(" "),
        diagnostics,
        repairs,
        skipped_files,
        safety_levels,
        frozen_callables: frozen_callables
            .into_iter()
            .map(|frozen| FrozenCallableWire {
                name: frozen.name,
                reason: frozen.reason,
            })
            .collect(),
    })
}

fn collect_file_candidates(
    analysis: &mut AnalysisDatabase,
    file: &Path,
    safety_ceiling: Option<RepairSafety>,
    cross_file_imports: &HashSet<String>,
    module_graph: &harn_modules::ModuleGraph,
    options: &FixOptions,
    out: &mut Vec<RepairCandidate>,
    escape: &mut ValueEscape<'_>,
) -> Result<(), SkippedFileWire> {
    let path_str = file.to_string_lossy().into_owned();
    let mut config = package::load_check_config(Some(file));
    commands::check::apply_harn_lint_config(file, &mut config);
    let output = commands::check::analyze_file(analysis, file, &config, module_graph)
        .map_err(|error| skipped_file_from_analysis_error(path_str.clone(), error))?;
    let source = output.source;
    let program = output.program;
    let exported_names = module_graph
        .exports_for_module(file)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let ambient_context = AmbientRepairContext {
        cross_module_importer_count: module_graph.importers_of(file).len(),
    };
    let deferred_capability_mismatches = if options.capability_migrations_only {
        deferred_capability_mismatch_spans(
            &output.diagnostics,
            &program,
            &source,
            &exported_names,
            escape.referenced_by_value,
        )
    } else {
        BTreeSet::new()
    };

    for diag in &output.diagnostics {
        if harn_lint::type_diagnostic_lint_disabled(diag, &config.disable_rules) {
            continue;
        }
        let unresolved_name = match diag.details.as_ref() {
            Some(DiagnosticDetails::UnresolvedName { name }) => Some(name.clone()),
            _ => None,
        };
        let (mut expected_type, actual_type) = match diag.details.as_ref() {
            Some(DiagnosticDetails::TypeMismatch { expected, actual }) => {
                (Some(expected.clone()), Some(actual.clone()))
            }
            _ => (None, None),
        };
        if let Some(DiagnosticDetails::CallArity {
            parameter_types,
            actual: 0,
            ..
        }) = diag.details.as_ref()
        {
            expected_type = parameter_types.first().cloned().flatten().filter(|expected| {
                matches!(expected, TypeExpr::Named(name) if name == "Harness" || harn_builtin_meta::CapabilityId::from_type_name(name).is_some())
            });
        }
        if diag
            .span
            .is_some_and(|span| deferred_capability_mismatches.contains(&(span.start, span.end)))
        {
            continue;
        }
        let synthesized = (diag.code == Code::UndefinedVariable
            && unresolved_name.as_deref() == Some("harness"))
        .then(|| {
            synthesize_missing_harness_repair(
                diag.span?,
                &source,
                &program,
                &exported_names,
                &ambient_context,
                escape,
            )
        })
        .flatten();
        let synthesized = synthesized.or_else(|| {
            if diag.code == Code::OrchestrationArity {
                return synthesize_missing_zero_arg_capability_repair(
                    diag.span?,
                    expected_type.as_ref()?,
                    &source,
                    &program,
                );
            }
            if diag.code != Code::ArgumentTypeMismatch {
                return None;
            }
            if matches!(expected_type.as_ref(), Some(TypeExpr::Named(expected)) if expected == "Harness") {
                return synthesize_missing_root_argument_repair(
                    diag.span?,
                    &source,
                    &program,
                    &exported_names,
                    &ambient_context,
                    escape,
                );
            }
            synthesize_missing_capability_argument_repair(
                diag.span?,
                expected_type.as_ref()?,
                actual_type.as_ref()?,
                &source,
                &program,
            )
            .or_else(|| {
                let TypeExpr::Named(expected) = expected_type.as_ref()? else {
                    return None;
                };
                harn_builtin_meta::CapabilityId::from_type_name(expected)?;
                synthesize_missing_root_argument_repair(
                    diag.span?,
                    &source,
                    &program,
                    &exported_names,
                    &ambient_context,
                    escape,
                )
            })
        });
        let (repair, edits, impact) = if let Some(repair) = synthesized {
            repair
        } else {
            let Some(repair) = diag.repair.clone() else {
                continue;
            };
            (
                repair,
                diag.fix.clone().unwrap_or_default(),
                RepairImpactWire::generic(),
            )
        };
        if !repair_allowed(&repair, safety_ceiling) {
            continue;
        }
        if options.capability_migrations_only && !is_capability_migration_repair(&repair) {
            continue;
        }
        out.push(RepairCandidate {
            file: path_str.clone(),
            source: "typecheck",
            severity: severity_label(diag.severity),
            code: diag.code,
            message: diag.message.clone(),
            unresolved_name,
            expected_type,
            span: diag.span,
            repair,
            impact,
            edits,
        });
    }

    let lint_context = FixLintContext::load(file);
    let lint_options = lint_context.options(file);
    let lint_diagnostics = harn_lint::lint_with_module_graph(
        &program,
        &config.disable_rules,
        Some(&source),
        cross_file_imports,
        module_graph,
        file,
        &lint_options,
    );
    for diag in &lint_diagnostics {
        let Some((repair, edits, impact)) =
            lint_candidate_repair(diag, file, &source, &program, module_graph, escape)
        else {
            continue;
        };
        if !repair_allowed(&repair, safety_ceiling) {
            continue;
        }
        if options.capability_migrations_only && !is_capability_migration_repair(&repair) {
            continue;
        }
        out.push(RepairCandidate {
            file: path_str.clone(),
            source: "lint",
            severity: lint_severity_label(diag.severity),
            code: diag.code,
            message: diag.message.clone(),
            unresolved_name: None,
            expected_type: None,
            span: Some(diag.span),
            repair,
            impact,
            edits,
        });
    }

    // Capability-only plans are a migration contract, not a general repair
    // census. Preflight repairs have no capability migration semantics and
    // must not appear as non-applicable work on every convergence pass.
    if !options.capability_migrations_only {
        collect_preflight(
            file,
            &source,
            &program,
            &config,
            module_graph,
            safety_ceiling,
            out,
        );
    }
    Ok(())
}

fn deferred_capability_mismatch_spans(
    diagnostics: &[harn_parser::TypeDiagnostic],
    program: &[SNode],
    source: &str,
    exported_names: &BTreeSet<String>,
    referenced_by_value: &BTreeSet<String>,
) -> BTreeSet<(usize, usize)> {
    let calls = collect_callable_infos(program, source, exported_names, referenced_by_value)
        .into_iter()
        .flat_map(|callable| callable.calls)
        .collect::<Vec<_>>();
    let mut by_call = BTreeMap::<(usize, usize), Vec<(usize, Span)>>::new();
    for diagnostic in diagnostics {
        if diagnostic.code != Code::ArgumentTypeMismatch {
            continue;
        }
        let Some(DiagnosticDetails::TypeMismatch {
            expected: TypeExpr::Named(expected),
            ..
        }) = diagnostic.details.as_ref()
        else {
            continue;
        };
        if expected != "Harness"
            && harn_builtin_meta::CapabilityId::from_type_name(expected.as_str()).is_none()
        {
            continue;
        }
        let Some(span) = diagnostic.span else {
            continue;
        };
        let Some((call, argument_index)) = calls.iter().find_map(|call| {
            call.args
                .iter()
                .position(|argument| argument.start == span.start && argument.end == span.end)
                .map(|index| (call, index))
        }) else {
            continue;
        };
        by_call
            .entry((call.span.start, call.span.end))
            .or_default()
            .push((argument_index, span));
    }

    let mut deferred = BTreeSet::new();
    for mismatches in by_call.values_mut() {
        mismatches.sort_by_key(|(argument_index, span)| (*argument_index, span.start, span.end));
        deferred.extend(
            mismatches
                .iter()
                .skip(1)
                .map(|(_, span)| (span.start, span.end)),
        );
    }
    deferred
}

fn skipped_file_from_analysis_error(
    path: String,
    error: commands::check::FileAnalysisError,
) -> SkippedFileWire {
    let (reason, diagnostic) = match error {
        commands::check::FileAnalysisError::Read(error) => (
            "read_error",
            SkippedFileDiagnosticWire {
                source: "io",
                severity: "error",
                code: None,
                message: format!("failed to read {path}: {error}"),
                span: None,
                help: None,
            },
        ),
        commands::check::FileAnalysisError::Analysis(AnalysisError::MissingSource(id)) => (
            "analysis_error",
            SkippedFileDiagnosticWire {
                source: "analysis",
                severity: "error",
                code: None,
                message: format!("missing analysis source {}", id.as_str()),
                span: None,
                help: None,
            },
        ),
        commands::check::FileAnalysisError::Analysis(AnalysisError::Lex { error, .. }) => (
            "lex_error",
            SkippedFileDiagnosticWire {
                source: "lexer",
                severity: "error",
                code: Some(harn_parser::diagnostic::lexer_error_code(&error).to_string()),
                message: error.to_string(),
                span: Some(SpanWire::from(commands::check::span_from_lexer_error(
                    &error,
                ))),
                help: None,
            },
        ),
        commands::check::FileAnalysisError::Analysis(AnalysisError::Parse { errors, .. }) => {
            let error = errors
                .first()
                .expect("analysis parse errors should include at least one error");
            (
                "parse_error",
                SkippedFileDiagnosticWire {
                    source: "parser",
                    severity: "error",
                    code: Some(harn_parser::diagnostic::parser_error_code(error).to_string()),
                    message: harn_parser::diagnostic::parser_error_message(error),
                    span: Some(SpanWire::from(commands::check::span_from_parser_error(
                        error,
                    ))),
                    help: harn_parser::diagnostic::parser_error_help(error).map(str::to_string),
                },
            )
        }
    };
    SkippedFileWire {
        path,
        reason,
        diagnostics: vec![diagnostic],
    }
}

fn lint_candidate_repair(
    diag: &harn_lint::LintDiagnostic,
    file: &Path,
    source: &str,
    program: &[SNode],
    module_graph: &harn_modules::ModuleGraph,
    escape: &mut ValueEscape<'_>,
) -> Option<(Repair, Vec<FixEdit>, RepairImpactWire)> {
    if ambient_capability_handle(diag.code).is_some() {
        let exported_names = module_graph
            .exports_for_module(file)
            .into_iter()
            .collect::<BTreeSet<_>>();
        let context = AmbientRepairContext {
            cross_module_importer_count: module_graph.importers_of(file).len(),
        };
        return synthesize_ambient_capability_repair(
            diag,
            source,
            program,
            &exported_names,
            &context,
            escape,
        );
    }
    let repair = diag.repair()?;
    Some((
        repair,
        diag.fix.clone().unwrap_or_default(),
        RepairImpactWire::generic(),
    ))
}

fn synthesize_ambient_capability_repair(
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

fn synthesize_missing_capability_argument_repair(
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

fn synthesize_missing_zero_arg_capability_repair(
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

fn capability_bundle_literal(expected: &TypeExpr, binding: &str) -> Option<String> {
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
fn push_signature_edits(edits: &mut Vec<FixEdit>, source: &str, info: &CallableInfo) -> Option<()> {
    edits.push(add_harness_param_edit(source, info)?);
    for alias_edit in &info.alias_widening_edits {
        if !edits.iter().any(|edit| edit.span == alias_edit.span) {
            edits.push(alias_edit.clone());
        }
    }
    Some(())
}

fn synthesize_missing_harness_repair(
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

fn synthesize_missing_root_argument_repair(
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

fn repair_impact_for_signature_threading(
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

fn dedupe_edits(edits: Vec<FixEdit>) -> Vec<FixEdit> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for edit in edits {
        let key = (edit.span.start, edit.span.end, edit.replacement.clone());
        if seen.insert(key) {
            out.push(edit);
        }
    }
    out
}

fn dedupe_wire_edits(edits: &[FixEditWire]) -> Vec<FixEditWire> {
    let mut seen = BTreeSet::new();
    let mut out: Vec<FixEditWire> = Vec::new();
    for edit in edits {
        let key = (edit.span.start, edit.span.end, edit.replacement.clone());
        if seen.insert(key) {
            out.push(edit.clone());
        }
    }
    collapse_subsumed_insertions(out)
}

/// Drop an insertion whose replacement another insertion at the same offset
/// already contains as a prefix.
///
/// A call missing several capability carriers produces one repair per missing
/// argument and one whole-program repair supplying all of them, so the same
/// offset receives both `harness.env, ` and `harness.env, harness.fs, `. Those
/// are the same carriers in the same parameter order, not two candidate fixes:
/// the longer one satisfies every diagnostic the shorter one does. Treating
/// them as ambiguous rejects the file, and because a rejected candidate aborts
/// the whole pass, one multi-carrier callee blocks the migration of every other
/// file with it.
fn collapse_subsumed_insertions(edits: Vec<FixEditWire>) -> Vec<FixEditWire> {
    let subsumed: BTreeSet<usize> = edits
        .iter()
        .enumerate()
        .filter(|(_, edit)| edit.span.start == edit.span.end)
        .filter(|(index, edit)| {
            edits.iter().enumerate().any(|(other_index, other)| {
                other_index != *index
                    && other.span.start == other.span.end
                    && other.span.start == edit.span.start
                    && other.replacement.len() > edit.replacement.len()
                    && other.replacement.starts_with(&edit.replacement)
            })
        })
        .map(|(index, _)| index)
        .collect();
    edits
        .into_iter()
        .enumerate()
        .filter(|(index, _)| !subsumed.contains(index))
        .map(|(_, edit)| edit)
        .collect()
}

fn replace_identifier_within_span_fix(
    source: &str,
    span: Span,
    old: &str,
    new: &str,
) -> Option<Vec<FixEdit>> {
    let region = source.get(span.start..span.end)?;
    let offset = region.match_indices(old).find_map(|(offset, _)| {
        let before_ok = offset == 0
            || !region
                .as_bytes()
                .get(offset.wrapping_sub(1))
                .is_some_and(|byte| is_ident_byte(*byte));
        let end = offset + old.len();
        let after_ok = region
            .as_bytes()
            .get(end)
            .is_none_or(|byte| !is_ident_byte(*byte));
        (before_ok && after_ok).then_some(offset)
    })?;
    let start = span.start + offset;
    Some(vec![FixEdit {
        span: Span::with_offsets(start, start + old.len(), span.line, span.column + offset),
        replacement: new.to_string(),
    }])
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn collect_preflight(
    file: &Path,
    source: &str,
    program: &[SNode],
    config: &CheckConfig,
    module_graph: &harn_modules::ModuleGraph,
    safety_ceiling: Option<RepairSafety>,
    out: &mut Vec<RepairCandidate>,
) {
    let preflight_severity = PreflightSeverity::from_opt(config.preflight_severity.as_deref());
    if preflight_severity == PreflightSeverity::Off {
        return;
    }

    for diag in preflight_diagnostics(file, source, program, config, module_graph) {
        if commands::check::is_preflight_allowed(&diag.tags, &config.preflight_allow) {
            continue;
        }
        let Some(template) = diag.code.repair_template() else {
            continue;
        };
        let repair = Repair::from_template(template);
        if !repair_allowed(&repair, safety_ceiling) {
            continue;
        }
        out.push(RepairCandidate {
            file: diag.path,
            source: "preflight",
            severity: match preflight_severity {
                PreflightSeverity::Warning => "warning",
                PreflightSeverity::Error => "error",
                PreflightSeverity::Off => unreachable!(),
            },
            code: diag.code,
            message: diag.message,
            unresolved_name: None,
            expected_type: None,
            span: Some(diag.span),
            repair,
            impact: RepairImpactWire::generic(),
            edits: Vec::new(),
        });
    }
}

fn repair_allowed(repair: &Repair, safety_ceiling: Option<RepairSafety>) -> bool {
    safety_ceiling
        .map(|ceiling| repair.safety.is_at_most(ceiling))
        .unwrap_or(true)
}

fn detect_conflicts(candidates: &[RepairCandidate]) -> Vec<Vec<usize>> {
    // Candidates reach one plan from passes that spell a path differently —
    // the per-file lint pass reports `./src/workflow.harn` where the
    // whole-program capability pass reports an absolute path. Comparing raw
    // spellings declares two edits to the same physical file to be in
    // different files, so genuinely overlapping edits are both marked
    // `applies_cleanly` and merged, and the collision only surfaces at write
    // time as a hard failure. Resolve identity once per candidate, both to be
    // correct and to keep this quadratic scan free of filesystem calls.
    let keys = candidates
        .iter()
        .map(|candidate| apply::edit_group_key(&candidate.file))
        .collect::<Vec<_>>();
    let mut conflicts = vec![Vec::new(); candidates.len()];
    for left in 0..candidates.len() {
        for right in (left + 1)..candidates.len() {
            if keys[left] == keys[right]
                && candidates_overlap(&candidates[left], &candidates[right])
            {
                conflicts[left].push(right);
                conflicts[right].push(left);
            }
        }
    }
    conflicts
}

/// Whether two candidates for the *same* file touch overlapping source.
/// File identity is the caller's job — see [`detect_conflicts`].
fn candidates_overlap(left: &RepairCandidate, right: &RepairCandidate) -> bool {
    left.edits.iter().any(|left_edit| {
        right
            .edits
            .iter()
            .any(|right_edit| edits_conflict(left_edit, right_edit))
    })
}

fn edits_conflict(left: &FixEdit, right: &FixEdit) -> bool {
    if left.span == right.span && left.replacement == right.replacement {
        return false;
    }
    let same_zero_width = left.span.start == left.span.end
        && right.span.start == right.span.end
        && left.span.start == right.span.start;
    if same_zero_width {
        return left.replacement != right.replacement;
    }
    left.span.start < right.span.end && left.span.end > right.span.start
}

fn severity_label(severity: DiagnosticSeverity) -> &'static str {
    match severity {
        DiagnosticSeverity::Error => "error",
        DiagnosticSeverity::Warning => "warning",
    }
}

fn lint_severity_label(severity: LintSeverity) -> &'static str {
    match severity {
        LintSeverity::Info => "info",
        LintSeverity::Warning => "warning",
        LintSeverity::Error => "error",
    }
}

impl From<Span> for SpanWire {
    fn from(span: Span) -> Self {
        SpanWire {
            start: span.start,
            end: span.end,
            line: span.line,
            column: span.column,
            end_line: span.end_line,
        }
    }
}

impl From<&FixEdit> for FixEditWire {
    fn from(edit: &FixEdit) -> Self {
        FixEditWire {
            span: SpanWire::from(edit.span),
            replacement: edit.replacement.clone(),
        }
    }
}

impl From<&Repair> for RepairMetadataWire {
    fn from(repair: &Repair) -> Self {
        RepairMetadataWire {
            id: repair.id.as_str().to_string(),
            summary: repair.summary.clone(),
            safety: repair.safety.as_str().to_string(),
        }
    }
}

#[cfg(test)]
#[path = "fix/apply_tests.rs"]
mod apply_tests;
#[cfg(test)]
#[path = "fix/capability_apply_tests.rs"]
mod capability_apply_tests;
#[cfg(test)]
mod tests;
