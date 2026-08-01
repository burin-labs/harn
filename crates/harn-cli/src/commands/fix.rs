//! `harn fix`: propose or apply repair-bearing diagnostics.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use harn_lexer::{FixEdit, Span};
use harn_lint::LintSeverity;
use harn_parser::analysis::{AnalysisDatabase, AnalysisError};
use harn_parser::{
    visit, DiagnosticCode as Code, DiagnosticDetails, DiagnosticSeverity, Node, Repair,
    RepairSafety, SNode, TypeExpr,
};
use serde::Serialize;

use crate::cli::FixArgs;
use crate::commands;
use crate::commands::check::collect_preflight_diagnostics_with_module_graph as preflight_diagnostics;
use crate::package::{self, CheckConfig, PreflightSeverity};

#[path = "fix/capability_migrations.rs"]
mod capability_migrations;
#[path = "fix/lint_context.rs"]
mod lint_context;
#[path = "fix/signature_threading.rs"]
mod signature_threading;
#[path = "fix/whole_program_capabilities.rs"]
mod whole_program_capabilities;
use capability_migrations::{ambient_call_rewrite, ambient_capability_handle, ambient_replacement};
use lint_context::FixLintContext;
#[path = "fix/apply.rs"]
mod apply;
use apply::apply_repairs_with_options;
#[cfg(test)]
use apply::{apply_file_edits, apply_repairs, repair_path};
use signature_threading::{
    add_call_argument_edit, add_harness_param_edit, build_reverse_callers, collect_callable_infos,
    harness_param_name_for_insert, propagate_harness_requirements,
    repair_for_ambient_capability_plan,
};

pub(crate) const FIX_PLAN_SCHEMA_VERSION: u32 = 2;
pub(crate) const FIX_APPLY_SCHEMA_VERSION: u32 = 2;
const CAPABILITY_MIGRATION_MAX_PASSES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FixRunError {
    Command(String),
    PartialFailure(String),
}

impl FixRunError {
    pub(crate) fn message(&self) -> &str {
        match self {
            Self::Command(message) | Self::PartialFailure(message) => message,
        }
    }

    pub(crate) fn is_partial_failure(&self) -> bool {
        matches!(self, Self::PartialFailure(_))
    }
}

impl From<String> for FixRunError {
    fn from(message: String) -> Self {
        Self::Command(message)
    }
}

impl std::fmt::Display for FixRunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for FixRunError {}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RepairPlan {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub path: String,
    pub diagnostics: Vec<DiagnosticWire>,
    pub repairs: Vec<RepairWire>,
    #[serde(rename = "skippedFiles")]
    pub skipped_files: Vec<SkippedFileWire>,
    #[serde(rename = "safetyLevels")]
    pub safety_levels: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ApplyResult {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub applied: Vec<AppliedRepairWire>,
    pub skipped: Vec<SkippedRepairWire>,
    #[serde(rename = "skippedFiles")]
    pub skipped_files: Vec<SkippedFileWire>,
    #[serde(rename = "post_apply_diagnostics_count")]
    pub post_apply_diagnostics_count: usize,
    #[serde(rename = "dryRun")]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AppliedRepairWire {
    pub diagnostic_code: String,
    pub repair_id: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SkippedRepairWire {
    pub diagnostic_index: usize,
    pub diagnostic_code: String,
    pub repair_id: String,
    pub path: String,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SkippedFileWire {
    pub path: String,
    pub reason: &'static str,
    pub diagnostics: Vec<SkippedFileDiagnosticWire>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SkippedFileDiagnosticWire {
    pub source: &'static str,
    pub severity: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<SpanWire>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DiagnosticWire {
    pub index: usize,
    pub file: String,
    pub source: &'static str,
    pub severity: &'static str,
    pub code: String,
    pub message: String,
    pub span: Option<SpanWire>,
    pub repair: RepairMetadataWire,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RepairWire {
    pub diagnostic_index: usize,
    pub diagnostic_code: String,
    pub repair: RepairMetadataWire,
    pub impact: RepairImpactWire,
    pub edits: Vec<FixEditWire>,
    pub applies_cleanly: bool,
    pub conflicts_with: Vec<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RepairImpactWire {
    pub classification: String,
    pub strategy: Option<String>,
    #[serde(rename = "signatureChanges")]
    pub signature_changes: Vec<SignatureChangeWire>,
    #[serde(rename = "requiresCrossModuleCallerUpdates")]
    pub requires_cross_module_caller_updates: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct SignatureChangeWire {
    pub callable: String,
    #[serde(rename = "isExported")]
    pub is_exported: bool,
    #[serde(rename = "isEntrypoint")]
    pub is_entrypoint: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RepairMetadataWire {
    pub id: String,
    pub summary: String,
    pub safety: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct FixEditWire {
    pub span: SpanWire,
    pub replacement: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub(crate) struct SpanWire {
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub column: usize,
    pub end_line: usize,
}

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
    can_add_harness_param: bool,
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

#[derive(Debug, Clone, Copy, Default)]
struct FixOptions {
    capability_migrations_only: bool,
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

pub(crate) fn run(args: &FixArgs) -> Result<(), FixRunError> {
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
            &args.path,
            safety,
            args.dry_run,
            FixOptions {
                capability_migrations_only: args.capability_migrations_only,
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
        &args.path,
        args.safety,
        FixOptions {
            capability_migrations_only: args.capability_migrations_only,
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

#[cfg(test)]
pub(crate) fn build_plan(
    target: &Path,
    safety_ceiling: Option<RepairSafety>,
) -> Result<RepairPlan, String> {
    build_plan_with_options(target, safety_ceiling, FixOptions::default())
}

fn build_plan_with_options(
    target: &Path,
    safety_ceiling: Option<RepairSafety>,
    options: FixOptions,
) -> Result<RepairPlan, String> {
    if let Err(error) = package::validate_runtime_manifest_extensions(target) {
        return Err(format!("manifest extension validation failed: {error}"));
    }

    let target_string = target.to_string_lossy().into_owned();
    let target_refs = [target_string.as_str()];
    let files = commands::check::collect_harn_targets(&target_refs);
    if files.is_empty() {
        return Err("no .harn files found under the given target".to_string());
    }

    let module_graph = commands::check::build_module_graph(&files);
    let cross_file_imports = commands::check::collect_cross_file_imports(&module_graph);
    let mut analysis = AnalysisDatabase::new();
    let mut candidates = Vec::new();
    let mut skipped_files = Vec::new();
    for file in &files {
        if let Err(skipped) = collect_file_candidates(
            &mut analysis,
            file,
            safety_ceiling,
            &cross_file_imports,
            &module_graph,
            options,
            &mut candidates,
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
    let whole_program_repairs =
        whole_program_capabilities::plan(&valid_files, &module_graph, &candidates)?;
    if !whole_program_repairs.is_empty() {
        let whole_program_files = whole_program_repairs
            .iter()
            .map(|repair| {
                std::fs::canonicalize(&repair.file)
                    .unwrap_or_else(|_| Path::new(&repair.file).to_path_buf())
            })
            .collect::<BTreeSet<_>>();
        candidates.retain(|candidate| {
            if is_whole_program_superseded_repair(&candidate.repair) {
                return false;
            }
            // A binding that looked unused before the program plan may be the
            // carrier used by its emitted call rewrites. Defer that cleanup to
            // the next plan instead of renaming the binding in the same pass.
            candidate.repair.id.as_str() != "bindings/rename-unused"
                || !whole_program_files.contains(
                    &std::fs::canonicalize(&candidate.file)
                        .unwrap_or_else(|_| Path::new(&candidate.file).to_path_buf()),
                )
        });
        candidates.extend(whole_program_repairs);
    }

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
        path: target_string,
        diagnostics,
        repairs,
        skipped_files,
        safety_levels,
    })
}

fn collect_file_candidates(
    analysis: &mut AnalysisDatabase,
    file: &Path,
    safety_ceiling: Option<RepairSafety>,
    cross_file_imports: &HashSet<String>,
    module_graph: &harn_modules::ModuleGraph,
    options: FixOptions,
    out: &mut Vec<RepairCandidate>,
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

    for diag in &output.diagnostics {
        if harn_lint::type_diagnostic_lint_disabled(diag, &config.disable_rules) {
            continue;
        }
        let unresolved_name = match diag.details.as_ref() {
            Some(DiagnosticDetails::UnresolvedName { name }) => Some(name.clone()),
            _ => None,
        };
        let (expected_type, actual_type) = match diag.details.as_ref() {
            Some(DiagnosticDetails::TypeMismatch { expected, actual }) => {
                (Some(expected.clone()), Some(actual.clone()))
            }
            _ => (None, None),
        };
        let synthesized = (diag.code == Code::UndefinedVariable
            && unresolved_name.as_deref() == Some("harness"))
        .then(|| {
            synthesize_missing_harness_repair(
                diag.span?,
                &source,
                &program,
                &exported_names,
                &ambient_context,
            )
        })
        .flatten();
        let synthesized = synthesized.or_else(|| {
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
            lint_candidate_repair(diag, file, &source, &program, module_graph)
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

    collect_preflight(
        file,
        &source,
        &program,
        &config,
        module_graph,
        safety_ceiling,
        out,
    );
    Ok(())
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

fn is_capability_migration_repair(repair: &Repair) -> bool {
    let id = repair.id.as_str();
    id.starts_with("bindings/thread-harness")
        || matches!(
            id,
            "bindings/thread-missing-harness"
                | "bindings/thread-root-argument"
                | "bindings/prepend-capability-argument"
                | "bindings/attenuate-harness"
                | "bindings/attenuate-capability-argument"
                | "bindings/attenuate-capability-bundle-argument"
        )
}

fn is_whole_program_superseded_repair(repair: &Repair) -> bool {
    let id = repair.id.as_str();
    id.starts_with("bindings/thread-harness")
        || matches!(
            id,
            "bindings/thread-missing-harness"
                | "bindings/thread-root-argument"
                | "bindings/prepend-capability-argument"
        )
}

fn lint_candidate_repair(
    diag: &harn_lint::LintDiagnostic,
    file: &Path,
    source: &str,
    program: &[SNode],
    module_graph: &harn_modules::ModuleGraph,
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
) -> Option<(Repair, Vec<FixEdit>, RepairImpactWire)> {
    ambient_capability_handle(diag.code)?;
    let infos = collect_callable_infos(program, source, exported_names);
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
        edits.push(add_harness_param_edit(info)?);
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
            if candidate.span.start == span.start && candidate.span.end == span.end {
                if let Node::Identifier(binding) = &candidate.node {
                    matched_argument = Some((candidate.span, binding.clone()));
                }
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

fn insert_call_argument_before_span(
    source: &str,
    program: &[SNode],
    span: Span,
    argument: &str,
) -> Option<FixEdit> {
    let mut edit = None;
    visit::walk_program(program, &mut |node| {
        let Node::FunctionCall { args, .. } = &node.node else {
            return;
        };
        let Some(index) = args.iter().position(|candidate| {
            candidate.span.start == span.start && candidate.span.end == span.end
        }) else {
            return;
        };
        edit = if index == 0 {
            add_call_argument_edit(source, &node.span, argument)
        } else {
            let previous = args[index - 1].span;
            Some(FixEdit {
                span: Span::with_offsets(
                    previous.end,
                    previous.end,
                    previous.end_line,
                    previous.column,
                ),
                replacement: format!(", {argument}"),
            })
        };
    });
    edit
}

fn capability_argument_for_span(program: &[SNode], span: Span, expected: &str) -> Option<String> {
    let capability = harn_builtin_meta::CapabilityId::from_type_name(expected)?;
    let field_name = capability.field_name();
    let mut candidates = Vec::new();
    visit::walk_program(program, &mut |node| {
        let params = match &node.node {
            Node::FnDecl { params, .. }
            | Node::ToolDecl { params, .. }
            | Node::Pipeline { params, .. }
                if node.span.start <= span.start && node.span.end >= span.end =>
            {
                params
            }
            _ => return,
        };
        let mut direct = None;
        let mut bundled = None;
        let mut root = None;
        for param in params {
            match param.type_expr.as_ref() {
                Some(TypeExpr::Named(name)) if name == expected => {
                    direct = Some(param.name.clone());
                }
                Some(TypeExpr::Named(name)) if name == "Harness" => {
                    root = Some(format!("{}.{}", param.name, field_name));
                }
                Some(TypeExpr::Shape(fields))
                    if fields.iter().any(|field| {
                        field.name == field_name
                            && matches!(&field.type_expr, TypeExpr::Named(name) if name == expected)
                    }) =>
                {
                    bundled = Some(format!("{}.{}", param.name, field_name));
                }
                _ => {}
            }
        }
        if let Some(argument) = direct.or(bundled).or(root) {
            candidates.push((node.span.end.saturating_sub(node.span.start), argument));
        }
    });
    candidates.sort_by_key(|(width, _)| *width);
    candidates.into_iter().next().map(|(_, argument)| argument)
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

fn synthesize_missing_harness_repair(
    span: Span,
    source: &str,
    program: &[SNode],
    exported_names: &BTreeSet<String>,
    context: &AmbientRepairContext,
) -> Option<(Repair, Vec<FixEdit>, RepairImpactWire)> {
    let infos = collect_callable_infos(program, source, exported_names);
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
        edits.push(add_harness_param_edit(&infos[idx])?);
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
) -> Option<(Repair, Vec<FixEdit>, RepairImpactWire)> {
    let infos = collect_callable_infos(program, source, exported_names);
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
            edits.push(add_harness_param_edit(&infos[idx])?);
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
    let mut out = Vec::new();
    for edit in edits {
        let key = (edit.span.start, edit.span.end, edit.replacement.clone());
        if seen.insert(key) {
            out.push(edit.clone());
        }
    }
    out
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
    let mut conflicts = vec![Vec::new(); candidates.len()];
    for left in 0..candidates.len() {
        for right in (left + 1)..candidates.len() {
            if candidates_overlap(&candidates[left], &candidates[right]) {
                conflicts[left].push(right);
                conflicts[right].push(left);
            }
        }
    }
    conflicts
}

fn candidates_overlap(left: &RepairCandidate, right: &RepairCandidate) -> bool {
    if left.file != right.file {
        return false;
    }
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

fn print_human_plan(plan: &RepairPlan) {
    if plan.repairs.is_empty() && plan.skipped_files.is_empty() {
        println!("{}: no repairable diagnostics found", plan.path);
        return;
    }
    if !plan.repairs.is_empty() {
        println!(
            "{}: {} repairable diagnostic(s)",
            plan.path,
            plan.repairs.len()
        );
        println!(
            "idx  code          safety               edits  clean  impact                    repair"
        );
        for repair in &plan.repairs {
            let clean = if repair.applies_cleanly { "yes" } else { "no" };
            println!(
                "{:<4} {:<13} {:<20} {:<5} {:<5} {:<25} {}",
                repair.diagnostic_index,
                repair.diagnostic_code,
                repair.repair.safety,
                repair.edits.len(),
                clean,
                repair.impact.classification,
                repair.repair.id
            );
            for note in &repair.impact.notes {
                println!("      note: {note}");
            }
        }
    }
    print_skipped_files(&plan.skipped_files);
}

fn print_skipped_files(skipped_files: &[SkippedFileWire]) {
    if skipped_files.is_empty() {
        return;
    }
    println!("skipped {} file(s):", skipped_files.len());
    for skipped in skipped_files {
        println!("skipped {}: {}", skipped.path, skipped.reason);
        for diagnostic in &skipped.diagnostics {
            let code = diagnostic.code.as_deref().unwrap_or("no-code");
            println!("  {}[{}]: {}", diagnostic.source, code, diagnostic.message);
            if let Some(help) = &diagnostic.help {
                println!("    help: {help}");
            }
        }
    }
}

fn print_apply_result(result: &ApplyResult) {
    let verb = if result.dry_run {
        "would apply"
    } else {
        "applied"
    };
    println!(
        "{verb} {} repair(s), skipped {}; post-apply diagnostics: {}",
        result.applied.len(),
        result.skipped.len(),
        result.post_apply_diagnostics_count
    );
    for skipped in &result.skipped {
        println!(
            "skipped {} {} in {}: {}",
            skipped.diagnostic_code, skipped.repair_id, skipped.path, skipped.reason
        );
    }
    print_skipped_files(&result.skipped_files);
}

fn skipped_files_error(count: usize) -> String {
    format!("harn fix skipped {count} file(s) due to read, lex, or parse errors")
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
#[path = "fix/capability_apply_tests.rs"]
mod capability_apply_tests;
#[cfg(test)]
mod tests;
