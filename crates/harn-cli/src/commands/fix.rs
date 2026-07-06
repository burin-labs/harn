//! `harn fix`: propose or apply repair-bearing diagnostics.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use harn_lexer::{FixEdit, Span};
use harn_lint::LintSeverity;
use harn_parser::analysis::{AnalysisDatabase, AnalysisError};
use harn_parser::{
    visit, BindingPattern, DiagnosticCode as Code, DiagnosticSeverity, Node, Repair, RepairSafety,
    SNode, TypeExpr, TypedParam,
};
use serde::Serialize;

use crate::cli::{FixArgs, HarnessThreadingMode};
use crate::commands;
use crate::package::{self, CheckConfig, PreflightSeverity};

pub(crate) const FIX_PLAN_SCHEMA_VERSION: u32 = 2;
pub(crate) const FIX_APPLY_SCHEMA_VERSION: u32 = 2;

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
    #[serde(rename = "harnessThreading")]
    pub harness_threading: String,
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
}

#[derive(Debug, Clone)]
struct AmbientCapabilityCall {
    name: String,
    code: Code,
    span: Span,
}

#[derive(Debug, Clone, Copy, Default)]
struct FixOptions {
    harness_threading: HarnessThreadingMode,
}

struct AmbientRepairContext {
    harness_threading: HarnessThreadingMode,
    allow_stdlib_public_global: bool,
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
                harness_threading: args.harness_threading,
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
            harness_threading: args.harness_threading,
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
        harness_threading: options.harness_threading.as_str().to_string(),
        diagnostics,
        repairs,
        skipped_files,
        safety_levels,
    })
}

#[cfg(test)]
pub(crate) fn apply_repairs(
    target: &Path,
    safety_ceiling: RepairSafety,
    dry_run: bool,
) -> Result<ApplyResult, String> {
    apply_repairs_with_options(target, safety_ceiling, dry_run, FixOptions::default())
}

fn apply_repairs_with_options(
    target: &Path,
    safety_ceiling: RepairSafety,
    dry_run: bool,
    options: FixOptions,
) -> Result<ApplyResult, String> {
    let plan = build_plan_with_options(target, None, options)?;
    let mut edits_by_file: BTreeMap<String, Vec<FixEditWire>> = BTreeMap::new();
    let mut applied = Vec::new();
    let mut skipped = Vec::new();

    for repair in &plan.repairs {
        let path = repair_path(&plan, repair)?;
        let repair_safety = repair.repair.safety.parse::<RepairSafety>().map_err(|_| {
            format!(
                "internal error: unknown repair safety `{}`",
                repair.repair.safety
            )
        })?;

        let skip_reason = if repair_safety == RepairSafety::NeedsHuman {
            Some("needs_human")
        } else if !repair_safety.is_at_most(safety_ceiling) {
            Some("above_safety_ceiling")
        } else if !repair.applies_cleanly {
            Some("conflict")
        } else if repair.edits.is_empty() {
            Some("no_edits")
        } else {
            None
        };

        if let Some(reason) = skip_reason {
            skipped.push(SkippedRepairWire {
                diagnostic_index: repair.diagnostic_index,
                diagnostic_code: repair.diagnostic_code.clone(),
                repair_id: repair.repair.id.clone(),
                path,
                reason,
            });
            continue;
        }

        edits_by_file
            .entry(path.clone())
            .or_default()
            .extend(repair.edits.iter().cloned());
        applied.push(AppliedRepairWire {
            diagnostic_code: repair.diagnostic_code.clone(),
            repair_id: repair.repair.id.clone(),
            path,
        });
    }

    if !dry_run {
        for (path, edits) in &edits_by_file {
            let edits = dedupe_wire_edits(edits);
            apply_file_edits(Path::new(path), &edits)?;
        }
    }

    let remaining = count_remaining_diagnostics(target)?;
    Ok(ApplyResult {
        schema_version: FIX_APPLY_SCHEMA_VERSION,
        applied,
        skipped,
        skipped_files: remaining.skipped_files,
        post_apply_diagnostics_count: remaining.count,
        dry_run,
    })
}

fn repair_path(plan: &RepairPlan, repair: &RepairWire) -> Result<String, String> {
    plan.diagnostics
        .get(repair.diagnostic_index)
        .map(|diagnostic| diagnostic.file.clone())
        .ok_or_else(|| {
            format!(
                "internal error: repair references missing diagnostic index {}",
                repair.diagnostic_index
            )
        })
}

fn apply_file_edits(path: &Path, edits: &[FixEditWire]) -> Result<(), String> {
    if edits.is_empty() {
        return Ok(());
    }
    let mut result = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let mut sorted = edits.to_vec();
    sorted.sort_by_key(|edit| std::cmp::Reverse(edit.span.start));
    for edit in sorted {
        if edit.span.start > edit.span.end || edit.span.end > result.len() {
            return Err(format!(
                "repair edit span {}..{} is outside {} ({} bytes)",
                edit.span.start,
                edit.span.end,
                path.display(),
                result.len()
            ));
        }
        if !result.is_char_boundary(edit.span.start) || !result.is_char_boundary(edit.span.end) {
            return Err(format!(
                "repair edit span {}..{} is not on UTF-8 character boundaries in {}",
                edit.span.start,
                edit.span.end,
                path.display()
            ));
        }
        result.replace_range(edit.span.start..edit.span.end, &edit.replacement);
    }
    std::fs::write(path, result)
        .map_err(|error| format!("failed to write {}: {error}", path.display()))
}

#[derive(Debug, Clone)]
struct RemainingDiagnostics {
    count: usize,
    skipped_files: Vec<SkippedFileWire>,
}

fn count_remaining_diagnostics(target: &Path) -> Result<RemainingDiagnostics, String> {
    if let Err(error) = package::validate_runtime_manifest_extensions(target) {
        return Err(format!("manifest extension validation failed: {error}"));
    }

    let target_string = target.to_string_lossy().into_owned();
    let target_refs = [target_string.as_str()];
    let files = commands::check::collect_harn_targets(&target_refs);
    let module_graph = commands::check::build_module_graph(&files);
    let cross_file_imports = commands::check::collect_cross_file_imports(&module_graph);
    let mut analysis = AnalysisDatabase::new();
    let mut count = 0;
    let mut skipped_files = Vec::new();

    for file in &files {
        let mut config = package::load_check_config(Some(file));
        commands::check::apply_harn_lint_config(file, &mut config);
        let output =
            match commands::check::analyze_file(&mut analysis, file, &config, &module_graph) {
                Ok(output) => output,
                Err(skipped) => {
                    skipped_files.push(skipped_file_from_analysis_error(
                        file.to_string_lossy().into_owned(),
                        skipped,
                    ));
                    continue;
                }
            };
        let source = output.source;
        let program = output.program;

        count += output
            .diagnostics
            .iter()
            .filter(|diag| !harn_lint::type_diagnostic_lint_disabled(diag, &config.disable_rules))
            .count();

        let persona_step_allowlist = commands::check::harn_lint_persona_step_allowlist(file);
        let engine_rules = commands::check::project_engine_rule_sources(file);
        let native_rule_paths = commands::check::project_native_rule_paths(file);
        let options = harn_lint::LintOptions {
            file_path: Some(file),
            require_file_header: commands::check::harn_lint_require_file_header(file),
            require_docstrings: commands::check::harn_lint_require_docstrings(file),
            complexity_threshold: commands::check::harn_lint_complexity_threshold(file),
            persona_step_allowlist: &persona_step_allowlist,
            require_stdlib_metadata: commands::check::path_is_stdlib_source(file),
            engine_rules: &engine_rules,
            native_rule_paths: &native_rule_paths,
            severity_overrides: commands::check::harn_lint_severity_overrides(file),
        };
        count += harn_lint::lint_with_module_graph(
            &program,
            &config.disable_rules,
            Some(&source),
            &cross_file_imports,
            &module_graph,
            file,
            &options,
        )
        .len();

        let preflight_severity = PreflightSeverity::from_opt(config.preflight_severity.as_deref());
        if preflight_severity != PreflightSeverity::Off {
            count +=
                commands::check::collect_preflight_diagnostics(file, &source, &program, &config)
                    .into_iter()
                    .filter(|diag| {
                        !commands::check::is_preflight_allowed(&diag.tags, &config.preflight_allow)
                    })
                    .count();
        }
    }

    Ok(RemainingDiagnostics {
        count,
        skipped_files,
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

    for diag in &output.diagnostics {
        if harn_lint::type_diagnostic_lint_disabled(diag, &config.disable_rules) {
            continue;
        }
        let Some(repair) = diag.repair.clone() else {
            continue;
        };
        if !repair_allowed(&repair, safety_ceiling) {
            continue;
        }
        out.push(RepairCandidate {
            file: path_str.clone(),
            source: "typecheck",
            severity: severity_label(diag.severity),
            code: diag.code,
            message: diag.message.clone(),
            span: diag.span,
            repair,
            impact: RepairImpactWire::generic(),
            edits: diag.fix.clone().unwrap_or_default(),
        });
    }

    let persona_step_allowlist = commands::check::harn_lint_persona_step_allowlist(file);
    let engine_rules = commands::check::project_engine_rule_sources(file);
    let native_rule_paths = commands::check::project_native_rule_paths(file);
    let lint_options = harn_lint::LintOptions {
        file_path: Some(file),
        require_file_header: commands::check::harn_lint_require_file_header(file),
        require_docstrings: commands::check::harn_lint_require_docstrings(file),
        complexity_threshold: commands::check::harn_lint_complexity_threshold(file),
        persona_step_allowlist: &persona_step_allowlist,
        require_stdlib_metadata: commands::check::path_is_stdlib_source(file),
        engine_rules: &engine_rules,
        native_rule_paths: &native_rule_paths,
        severity_overrides: commands::check::harn_lint_severity_overrides(file),
    };
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
            lint_candidate_repair(diag, file, &source, &program, module_graph, options)
        else {
            continue;
        };
        if !repair_allowed(&repair, safety_ceiling) {
            continue;
        }
        out.push(RepairCandidate {
            file: path_str.clone(),
            source: "lint",
            severity: lint_severity_label(diag.severity),
            code: diag.code,
            message: diag.message.clone(),
            span: Some(diag.span),
            repair,
            impact,
            edits,
        });
    }

    collect_preflight_candidates(file, &source, &program, &config, safety_ceiling, out);
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

fn lint_candidate_repair(
    diag: &harn_lint::LintDiagnostic,
    file: &Path,
    source: &str,
    program: &[SNode],
    module_graph: &harn_modules::ModuleGraph,
    options: FixOptions,
) -> Option<(Repair, Vec<FixEdit>, RepairImpactWire)> {
    if ambient_capability_handle(diag.code).is_some() {
        let exported_names = module_graph
            .exports_for_module(file)
            .into_iter()
            .collect::<BTreeSet<_>>();
        let context = AmbientRepairContext {
            harness_threading: options.harness_threading,
            allow_stdlib_public_global: commands::check::path_is_stdlib_source(file),
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
    let owner_uses_global_harness = should_use_global_harness(owner, context);
    let replacement_binding = if owner_uses_global_harness {
        Some("harness".to_string())
    } else {
        None
    }
    .or_else(|| {
        owner
            .harness_binding
            .clone()
            .or_else(|| harness_param_name_for_insert(owner).map(str::to_string))
    });
    let replacement =
        ambient_replacement(diag.code, &ambient.name, replacement_binding.as_deref())?;
    let mut edits =
        replace_identifier_within_span_fix(source, diag.span, &ambient.name, &replacement)?;

    if owner.harness_binding.is_some() {
        return Some((
            Repair::from_template(diag.code.repair_template()?),
            edits,
            RepairImpactWire::local_ambient("existing-harness-binding"),
        ));
    }
    if owner_uses_global_harness {
        return Some((
            use_enclosing_harness_global_repair(diag.code)?,
            edits,
            RepairImpactWire::local_ambient("use-enclosing-harness-global"),
        ));
    }

    let needed = propagate_harness_requirements(&infos, &reverse_callers, owner_idx, context);
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
                None if should_use_global_harness(caller, context) => "harness",
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

fn should_use_global_harness(info: &CallableInfo, context: &AmbientRepairContext) -> bool {
    if info.bound_names.contains("harness") {
        return false;
    }
    match context.harness_threading {
        HarnessThreadingMode::LocalGlobal => true,
        HarnessThreadingMode::ThreadParams => {
            context.allow_stdlib_public_global && info.is_exported
        }
    }
}

fn use_enclosing_harness_global_repair(code: Code) -> Option<Repair> {
    let capability = ambient_capability_handle(code)?;
    Some(Repair {
        id: harn_parser::RepairId::from_owned(
            "bindings/use-enclosing-harness-global".to_string(),
        ),
        summary: format!(
            "Use the VM-level `harness` binding for ambient {capability} calls without changing helper signatures"
        ),
        safety: RepairSafety::ScopeLocal,
    })
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

fn ambient_capability_handle(code: Code) -> Option<&'static str> {
    match code {
        Code::LintAmbientClockBuiltin => Some("clock"),
        Code::LintAmbientStdioBuiltin => Some("stdio"),
        Code::LintAmbientFsBuiltin => Some("fs"),
        Code::LintAmbientEnvBuiltin => Some("env"),
        Code::LintAmbientRandomBuiltin => Some("random"),
        Code::LintAmbientNetBuiltin => Some("net"),
        _ => None,
    }
}

fn ambient_code_for_call(name: &str, arg_count: usize) -> Option<Code> {
    if harn_parser::diagnostic::harness_clock_replacement(name).is_some() {
        return Some(Code::LintAmbientClockBuiltin);
    }
    if harn_parser::diagnostic::harness_stdio_replacement(name).is_some() {
        return Some(Code::LintAmbientStdioBuiltin);
    }
    if harn_parser::diagnostic::harness_fs_replacement(name).is_some() {
        return Some(Code::LintAmbientFsBuiltin);
    }
    if harn_parser::diagnostic::harness_env_replacement(name).is_some() {
        return Some(Code::LintAmbientEnvBuiltin);
    }
    if harn_parser::diagnostic::harness_random_replacement(name).is_some()
        && !is_explicit_seeded_random_call(name, arg_count)
    {
        return Some(Code::LintAmbientRandomBuiltin);
    }
    if harn_parser::diagnostic::harness_net_replacement(name).is_some() {
        return Some(Code::LintAmbientNetBuiltin);
    }
    None
}

fn is_explicit_seeded_random_call(name: &str, arg_count: usize) -> bool {
    matches!(
        (name, arg_count),
        ("random", 1) | ("random_int", 3) | ("random_choice", 2) | ("random_shuffle", 2)
    )
}

fn ambient_replacement(code: Code, name: &str, binding: Option<&str>) -> Option<String> {
    let replacement = match code {
        Code::LintAmbientClockBuiltin => harn_parser::diagnostic::harness_clock_replacement(name),
        Code::LintAmbientStdioBuiltin => harn_parser::diagnostic::harness_stdio_replacement(name),
        Code::LintAmbientFsBuiltin => harn_parser::diagnostic::harness_fs_replacement(name),
        Code::LintAmbientEnvBuiltin => harn_parser::diagnostic::harness_env_replacement(name),
        Code::LintAmbientRandomBuiltin => harn_parser::diagnostic::harness_random_replacement(name),
        Code::LintAmbientNetBuiltin => harn_parser::diagnostic::harness_net_replacement(name),
        _ => None,
    }?;
    Some(replacement.replacen("harness", binding.unwrap_or("harness"), 1))
}

fn collect_callable_infos(
    program: &[SNode],
    source: &str,
    exported_names: &BTreeSet<String>,
) -> Vec<CallableInfo> {
    let mut infos = Vec::new();
    for node in program {
        let inner = match &node.node {
            Node::AttributedDecl { inner, .. } => inner.as_ref(),
            _ => node,
        };
        match &inner.node {
            Node::FnDecl {
                name,
                params,
                body,
                is_pub,
                ..
            }
            | Node::ToolDecl {
                name,
                params,
                body,
                is_pub,
                ..
            } => {
                let mut calls = Vec::new();
                let mut ambient_capability_calls = Vec::new();
                visit_callable_body(inner, &mut |child| {
                    if let Node::FunctionCall { name, args, .. } = &child.node {
                        calls.push(CallSite {
                            callee: name.clone(),
                            span: child.span,
                        });
                        if let Some(code) = ambient_code_for_call(name, args.len()) {
                            ambient_capability_calls.push(AmbientCapabilityCall {
                                name: name.clone(),
                                code,
                                span: child.span,
                            });
                        }
                    }
                });
                let Some((insert_offset, has_params)) = callable_param_insert(source, inner.span)
                else {
                    continue;
                };
                let bound_names = callable_bound_names(params, body);
                infos.push(CallableInfo {
                    name: name.clone(),
                    span: inner.span,
                    is_exported: *is_pub || exported_names.contains(name),
                    insert_offset,
                    has_params: has_params || !params.is_empty(),
                    bound_names,
                    harness_binding: harness_param_name(params).map(str::to_string),
                    can_add_harness_param: true,
                    calls,
                    ambient_capability_calls,
                });
            }
            Node::Pipeline {
                name,
                params,
                body,
                is_pub,
                ..
            } => {
                let mut calls = Vec::new();
                let mut ambient_capability_calls = Vec::new();
                visit_callable_body(inner, &mut |child| {
                    if let Node::FunctionCall { name, args, .. } = &child.node {
                        calls.push(CallSite {
                            callee: name.clone(),
                            span: child.span,
                        });
                        if let Some(code) = ambient_code_for_call(name, args.len()) {
                            ambient_capability_calls.push(AmbientCapabilityCall {
                                name: name.clone(),
                                code,
                                span: child.span,
                            });
                        }
                    }
                });
                let Some((insert_offset, has_params)) = callable_param_insert(source, inner.span)
                else {
                    continue;
                };
                let param_names = params.iter().cloned().collect::<BTreeSet<_>>();
                let mut bound_names = param_names.clone();
                collect_binding_names(body, &mut bound_names);
                infos.push(CallableInfo {
                    name: name.clone(),
                    span: inner.span,
                    is_exported: *is_pub || exported_names.contains(name),
                    insert_offset,
                    has_params: has_params || !params.is_empty(),
                    bound_names,
                    harness_binding: Some("harness".to_string()),
                    can_add_harness_param: false,
                    calls,
                    ambient_capability_calls,
                });
            }
            _ => {}
        }
    }
    infos
}

fn callable_bound_names(params: &[TypedParam], body: &[SNode]) -> BTreeSet<String> {
    let mut names = params
        .iter()
        .map(|param| param.name.clone())
        .collect::<BTreeSet<_>>();
    collect_binding_names(body, &mut names);
    names
}

fn collect_binding_names(nodes: &[SNode], names: &mut BTreeSet<String>) {
    for node in nodes {
        visit::walk_node(node, &mut |child| match &child.node {
            Node::LetBinding { pattern, .. } | Node::ConstBinding { pattern, .. } => {
                collect_pattern_names(pattern, names);
            }
            Node::ForIn { pattern, .. } => {
                collect_pattern_names(pattern, names);
            }
            Node::Parallel {
                variable: Some(name),
                ..
            } => {
                names.insert(name.clone());
            }
            Node::TryCatch {
                error_var: Some(name),
                ..
            } => {
                names.insert(name.clone());
            }
            Node::Closure { params, .. } => {
                names.extend(params.iter().map(|param| param.name.clone()));
            }
            _ => {}
        });
    }
}

fn collect_pattern_names(pattern: &BindingPattern, names: &mut BTreeSet<String>) {
    match pattern {
        BindingPattern::Identifier(name) => {
            names.insert(name.clone());
        }
        BindingPattern::Dict(fields) => {
            names.extend(
                fields
                    .iter()
                    .map(|field| field.alias.as_ref().unwrap_or(&field.key).clone()),
            );
        }
        BindingPattern::List(elements) => {
            names.extend(elements.iter().map(|element| element.name.clone()));
        }
        BindingPattern::Pair(left, right) => {
            names.insert(left.clone());
            names.insert(right.clone());
        }
    }
}

fn visit_callable_body(node: &SNode, visitor: &mut impl FnMut(&SNode)) {
    let body = match &node.node {
        Node::FnDecl { body, .. } | Node::ToolDecl { body, .. } | Node::Pipeline { body, .. } => {
            body
        }
        _ => return,
    };
    for stmt in body {
        visit::walk_node(stmt, visitor);
    }
}

fn callable_param_insert(source: &str, span: Span) -> Option<(usize, bool)> {
    let region = source.get(span.start..span.end)?;
    let header_end = region.find('{').unwrap_or(region.len());
    let header = &region[..header_end];
    let open_paren = header.find('(')?;
    let close_paren = header[open_paren + 1..].find(')')? + open_paren + 1;
    let has_params = !header[open_paren + 1..close_paren].trim().is_empty();
    Some((span.start + open_paren + 1, has_params))
}

fn harness_param_name(params: &[TypedParam]) -> Option<&str> {
    params.iter().find_map(|param| {
        let TypeExpr::Named(name) = param.type_expr.as_ref()? else {
            return None;
        };
        if name == "Harness" && matches!(param.name.as_str(), "harness" | "_harness") {
            Some(param.name.as_str())
        } else {
            None
        }
    })
}

fn build_reverse_callers(infos: &[CallableInfo]) -> Vec<Vec<(usize, usize)>> {
    let by_name = infos
        .iter()
        .enumerate()
        .map(|(idx, info)| (info.name.as_str(), idx))
        .collect::<BTreeMap<_, _>>();
    let mut reverse = vec![Vec::new(); infos.len()];
    for (caller_idx, info) in infos.iter().enumerate() {
        for (call_idx, call) in info.calls.iter().enumerate() {
            let Some(&callee_idx) = by_name.get(call.callee.as_str()) else {
                continue;
            };
            reverse[callee_idx].push((caller_idx, call_idx));
        }
    }
    reverse
}

fn propagate_harness_requirements(
    infos: &[CallableInfo],
    reverse_callers: &[Vec<(usize, usize)>],
    owner_idx: usize,
    context: &AmbientRepairContext,
) -> BTreeSet<usize> {
    let mut needed = BTreeSet::from([owner_idx]);
    let mut changed = true;
    while changed {
        changed = false;
        let snapshot = needed.iter().copied().collect::<Vec<_>>();
        for callee_idx in snapshot {
            for &(caller_idx, _) in &reverse_callers[callee_idx] {
                if infos[caller_idx].harness_binding.is_none()
                    && !should_use_global_harness(&infos[caller_idx], context)
                    && infos[caller_idx].can_add_harness_param
                    && needed.insert(caller_idx)
                {
                    changed = true;
                }
            }
        }
    }
    needed
}

fn repair_for_ambient_capability_plan(
    code: Code,
    infos: &[CallableInfo],
    reverse_callers: &[Vec<(usize, usize)>],
    needed: &BTreeSet<usize>,
) -> Option<Repair> {
    let surface_changing = needed.iter().any(|&idx| {
        let info = &infos[idx];
        info.is_exported || info.name == "main" || reverse_callers[idx].is_empty()
    });
    if surface_changing {
        Some(Repair::from_template(
            Code::InvalidMainSignature.repair_template()?,
        ))
    } else {
        Some(Repair::from_template(code.repair_template()?))
    }
}

fn add_harness_param_edit(info: &CallableInfo) -> Option<FixEdit> {
    let name = harness_param_name_for_insert(info)?;
    Some(FixEdit {
        span: Span::with_offsets(
            info.insert_offset,
            info.insert_offset,
            info.span.line,
            info.span.column,
        ),
        replacement: if info.has_params {
            format!("{name}: Harness, ")
        } else {
            format!("{name}: Harness")
        },
    })
}

fn harness_param_name_for_insert(info: &CallableInfo) -> Option<&'static str> {
    if !info.bound_names.contains("harness") {
        return Some("harness");
    }
    if !info.bound_names.contains("_harness") {
        return Some("_harness");
    }
    None
}

fn add_call_argument_edit(source: &str, span: &Span, arg_name: &str) -> Option<FixEdit> {
    let region = source.get(span.start..span.end)?;
    let open_paren = region.find('(')?;
    let close_paren = region[open_paren + 1..].find(')')? + open_paren + 1;
    let has_args = !region[open_paren + 1..close_paren].trim().is_empty();
    let insert_at = span.start + open_paren + 1;
    Some(FixEdit {
        span: Span::with_offsets(insert_at, insert_at, span.line, span.column),
        replacement: if has_args {
            format!("{arg_name}, ")
        } else {
            arg_name.to_string()
        },
    })
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

fn collect_preflight_candidates(
    file: &Path,
    source: &str,
    program: &[SNode],
    config: &CheckConfig,
    safety_ceiling: Option<RepairSafety>,
    out: &mut Vec<RepairCandidate>,
) {
    let preflight_severity = PreflightSeverity::from_opt(config.preflight_severity.as_deref());
    if preflight_severity == PreflightSeverity::Off {
        return;
    }

    for diag in commands::check::collect_preflight_diagnostics(file, source, program, config) {
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
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn candidate(file: &str, start: usize, end: usize) -> RepairCandidate {
        RepairCandidate {
            file: file.to_string(),
            source: "typecheck",
            severity: "warning",
            code: Code::FormatterWouldReformat,
            message: "test".to_string(),
            span: Some(Span::with_offsets(start, end, 1, start + 1)),
            repair: Repair::from_template(Code::FormatterWouldReformat.repair_template().unwrap()),
            impact: RepairImpactWire::generic(),
            edits: vec![FixEdit {
                span: Span::with_offsets(start, end, 1, start + 1),
                replacement: "x".to_string(),
            }],
        }
    }

    #[test]
    fn conflict_detection_marks_overlapping_edits() {
        let conflicts = detect_conflicts(&[
            candidate("a.harn", 0, 3),
            candidate("a.harn", 2, 4),
            candidate("a.harn", 4, 5),
            candidate("b.harn", 2, 4),
        ]);
        assert_eq!(conflicts[0], vec![1]);
        assert_eq!(conflicts[1], vec![0]);
        assert!(conflicts[2].is_empty());
        assert!(conflicts[3].is_empty());
    }

    #[test]
    fn plan_reports_repairable_diagnostics_without_writing() {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join("repair_demo.harn");
        let source =
            "pipeline main() { let count = 1; let greeting = \"hello \" + count; greeting }\n";
        fs::write(&script, source).unwrap();
        let before = fs::read(&script).unwrap();

        let plan = build_plan(&script, Some(RepairSafety::BehaviorPreserving)).unwrap();

        assert_eq!(plan.schema_version, FIX_PLAN_SCHEMA_VERSION);
        assert!(
            plan.repairs.iter().any(|repair| {
                repair.repair.id == "style/string-interpolation"
                    && repair.repair.safety == "behavior-preserving"
                    && repair.applies_cleanly
            }),
            "expected string-interpolation repair in plan: {plan:#?}"
        );
        assert!(
            plan.repairs
                .iter()
                .all(|repair| repair.repair.safety != "needs-human"),
            "behavior-preserving ceiling must exclude needs-human repairs: {plan:#?}"
        );
        assert_eq!(fs::read(&script).unwrap(), before, "--plan must not write");

        let encoded = serde_json::to_value(&plan).unwrap();
        assert_eq!(encoded["schemaVersion"], FIX_PLAN_SCHEMA_VERSION);
        assert!(encoded["repairs"].as_array().is_some());
    }

    #[test]
    fn plan_skips_invalid_files_and_keeps_repairing_valid_files() {
        let temp = tempfile::TempDir::new().unwrap();
        let valid = temp.path().join("valid.harn");
        let invalid = temp.path().join("invalid.harn");
        fs::write(
            &valid,
            "pipeline main() { let count = 1; let greeting = \"hello \" + count; greeting }\n",
        )
        .unwrap();
        fs::write(&invalid, "fn bad() {\n").unwrap();

        let plan = build_plan(temp.path(), Some(RepairSafety::BehaviorPreserving)).unwrap();

        assert!(
            plan.repairs.iter().any(|repair| {
                repair.repair.id == "style/string-interpolation"
                    && repair_path(&plan, repair).unwrap() == valid.to_string_lossy().as_ref()
            }),
            "expected valid file repair despite invalid sibling: {plan:#?}"
        );
        assert_eq!(plan.skipped_files.len(), 1, "{plan:#?}");
        let skipped = &plan.skipped_files[0];
        assert_eq!(skipped.path, invalid.to_string_lossy().as_ref());
        assert_eq!(skipped.reason, "parse_error");
        assert_eq!(skipped.diagnostics[0].source, "parser");
        assert!(skipped.diagnostics[0].code.is_some());
        assert!(skipped.diagnostics[0].span.is_some());

        let encoded = serde_json::to_value(&plan).unwrap();
        assert_eq!(encoded["skippedFiles"][0]["reason"], "parse_error");
        assert!(encoded["skippedFiles"][0]["diagnostics"][0]["span"]["line"].is_u64());
    }

    #[test]
    fn apply_writes_clean_repairs_and_reports_post_check_count() {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join("repair_demo.harn");
        fs::write(
            &script,
            "pipeline main() { let count = 1; let greeting = \"hello \" + count; greeting }\n",
        )
        .unwrap();

        let result = apply_repairs(&script, RepairSafety::BehaviorPreserving, false).unwrap();

        assert_eq!(result.schema_version, FIX_APPLY_SCHEMA_VERSION);
        assert_eq!(result.applied.len(), 1, "{result:#?}");
        assert!(result.skipped.is_empty(), "{result:#?}");
        assert_eq!(result.post_apply_diagnostics_count, 0, "{result:#?}");
        let updated = fs::read_to_string(&script).unwrap();
        assert!(updated.contains("\"hello ${count}\""), "{updated}");
    }

    #[test]
    fn apply_directory_skips_invalid_files_after_applying_valid_files() {
        let temp = tempfile::TempDir::new().unwrap();
        let valid = temp.path().join("valid.harn");
        let invalid = temp.path().join("invalid.harn");
        fs::write(
            &valid,
            "pipeline main() { let count = 1; let greeting = \"hello \" + count; greeting }\n",
        )
        .unwrap();
        fs::write(&invalid, "fn bad() {\n").unwrap();

        let result = apply_repairs(temp.path(), RepairSafety::BehaviorPreserving, false).unwrap();

        assert_eq!(result.applied.len(), 1, "{result:#?}");
        assert_eq!(result.skipped_files.len(), 1, "{result:#?}");
        assert_eq!(
            result.skipped_files[0].path,
            invalid.to_string_lossy().as_ref()
        );
        assert_eq!(result.skipped_files[0].reason, "parse_error");
        assert_eq!(result.post_apply_diagnostics_count, 0, "{result:#?}");
        let updated = fs::read_to_string(&valid).unwrap();
        assert!(updated.contains("\"hello ${count}\""), "{updated}");
    }

    #[test]
    fn apply_dry_run_reports_without_writing() {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join("repair_demo.harn");
        let source =
            "pipeline main() { let count = 1; let greeting = \"hello \" + count; greeting }\n";
        fs::write(&script, source).unwrap();

        let result = apply_repairs(&script, RepairSafety::BehaviorPreserving, true).unwrap();

        assert!(result.dry_run);
        assert_eq!(result.applied.len(), 1, "{result:#?}");
        assert_eq!(fs::read_to_string(&script).unwrap(), source);
    }

    #[test]
    fn run_returns_error_after_reporting_skipped_files() {
        let temp = tempfile::TempDir::new().unwrap();
        fs::write(temp.path().join("invalid.harn"), "fn bad() {\n").unwrap();
        let args = FixArgs {
            plan: true,
            apply: false,
            dry_run: false,
            safety: None,
            harness_threading: HarnessThreadingMode::default(),
            json: false,
            path: temp.path().to_path_buf(),
        };

        let error = run(&args).unwrap_err();

        assert!(error.is_partial_failure(), "unexpected error: {error}");
        assert!(
            error.message().contains("skipped 1 file")
                && error.message().contains("read, lex, or parse errors"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn apply_skips_repairs_above_safety_ceiling() {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join("repair_demo.harn");
        let source =
            "pipeline main() { let count = 1; let greeting = \"hello \" + count; greeting }\n";
        fs::write(&script, source).unwrap();

        let result = apply_repairs(&script, RepairSafety::FormatOnly, false).unwrap();

        assert!(result.applied.is_empty(), "{result:#?}");
        assert!(
            result.skipped.iter().any(|skipped| {
                skipped.repair_id == "style/string-interpolation"
                    && skipped.reason == "above_safety_ceiling"
            }),
            "{result:#?}"
        );
        assert_eq!(fs::read_to_string(&script).unwrap(), source);
    }

    #[test]
    fn apply_rejects_needs_human_safety_ceiling() {
        let args = FixArgs {
            plan: false,
            apply: true,
            dry_run: false,
            safety: Some(RepairSafety::NeedsHuman),
            harness_threading: HarnessThreadingMode::default(),
            json: false,
            path: PathBuf::from("repair_demo.harn"),
        };

        let error = run(&args).unwrap_err();
        assert!(
            error.message().contains("needs-human")
                && error.message().contains("--plan --json")
                && !error.is_partial_failure(),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn plan_uses_global_harness_for_stdio_repairs_by_default() {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join("stdio_threading.harn");
        fs::write(
            &script,
            "fn helper() {\n  println(\"hi\")\n}\n\nfn main(harness: Harness) {\n  helper()\n}\n",
        )
        .unwrap();

        let plan = build_plan(&script, None).unwrap();
        let repair = plan
            .repairs
            .iter()
            .find(|repair| repair.diagnostic_code == Code::LintAmbientStdioBuiltin.to_string())
            .expect("ambient stdio repair should be present");

        assert_eq!(repair.repair.id, "bindings/use-enclosing-harness-global");
        assert_eq!(repair.repair.safety, "scope-local");
        assert_eq!(repair.impact.classification, "local-ambient-rewrite");
        assert!(repair.impact.signature_changes.is_empty());
        let replacements = repair
            .edits
            .iter()
            .map(|edit| edit.replacement.as_str())
            .collect::<Vec<_>>();
        assert!(
            replacements.contains(&"harness.stdio.println"),
            "expected direct call rewrite in edits: {replacements:?}"
        );
        assert!(
            !replacements
                .iter()
                .any(|replacement| replacement.contains("Harness")),
            "default repair should not change helper signatures: {replacements:?}"
        );
    }

    #[test]
    fn plan_marks_stdio_repairs_surface_changing_when_harness_is_unreachable() {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join("stdio_needs_param.harn");
        fs::write(&script, "pub fn helper() {\n  println(\"hi\")\n}\n").unwrap();

        let plan = build_plan_with_options(
            &script,
            None,
            FixOptions {
                harness_threading: HarnessThreadingMode::ThreadParams,
            },
        )
        .unwrap();
        let repair = plan
            .repairs
            .iter()
            .find(|repair| repair.diagnostic_code == Code::LintAmbientStdioBuiltin.to_string())
            .expect("ambient stdio repair should be present");

        assert_eq!(repair.repair.id, "bindings/thread-harness-needs-param");
        assert_eq!(repair.repair.safety, "surface-changing");
        assert_eq!(repair.impact.classification, "public-signature-change");
    }

    #[test]
    fn plan_json_reports_cross_module_public_signature_impact() {
        let temp = tempfile::TempDir::new().unwrap();
        let lib = temp.path().join("lib.harn");
        let entry = temp.path().join("main.harn");
        fs::write(
            &lib,
            "pub fn host_write_file(path: string, body: string) {\n  write_file(path, body)\n}\n",
        )
        .unwrap();
        fs::write(
            &entry,
            "import \"./lib\"\n\nfn main(harness: Harness) {\n  host_write_file(\"out.txt\", \"hi\")\n}\n",
        )
        .unwrap();

        let plan = build_plan_with_options(
            temp.path(),
            None,
            FixOptions {
                harness_threading: HarnessThreadingMode::ThreadParams,
            },
        )
        .unwrap();
        let repair_index = plan
            .repairs
            .iter()
            .position(|repair| {
                repair.diagnostic_code == Code::LintAmbientFsBuiltin.to_string()
                    && repair
                        .edits
                        .iter()
                        .any(|edit| edit.replacement == "harness: Harness, ")
            })
            .expect("public fs repair should be present");
        let repair = &plan.repairs[repair_index];

        assert_eq!(plan.harness_threading, "thread-params");
        assert_eq!(repair.impact.classification, "public-signature-change");
        assert!(repair.impact.requires_cross_module_caller_updates);
        assert_eq!(
            repair.impact.signature_changes,
            vec![SignatureChangeWire {
                callable: "host_write_file".to_string(),
                is_exported: true,
                is_entrypoint: false,
            }]
        );
        assert!(
            repair
                .impact
                .notes
                .iter()
                .any(|note| note.contains("cross-module callers must be updated")),
            "{repair:#?}"
        );

        let encoded = serde_json::to_value(&plan).unwrap();
        assert_eq!(
            encoded["repairs"][repair_index]["impact"]["classification"],
            "public-signature-change"
        );
    }

    #[test]
    fn apply_thread_params_threads_harness_for_stdio_migration() {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join("stdio_apply.harn");
        fs::write(
            &script,
            "pub fn helper() {\n  println(\"hi\")\n}\n\nfn main(harness: Harness) {\n  helper()\n}\n",
        )
        .unwrap();

        let result = apply_repairs_with_options(
            &script,
            RepairSafety::SurfaceChanging,
            false,
            FixOptions {
                harness_threading: HarnessThreadingMode::ThreadParams,
            },
        )
        .unwrap();
        assert!(
            result.applied.iter().any(|repair| {
                repair.diagnostic_code == Code::LintAmbientStdioBuiltin.to_string()
                    && repair.repair_id == "bindings/thread-harness-needs-param"
            }),
            "{result:#?}"
        );

        let updated = fs::read_to_string(&script).unwrap();
        assert!(
            updated.contains("fn helper(harness: Harness)"),
            "expected helper to gain a harness parameter: {updated}"
        );
        assert!(
            updated.contains("helper(harness)"),
            "expected main to thread harness into helper: {updated}"
        );
        assert!(
            updated.contains("harness.stdio.println(\"hi\")"),
            "expected ambient stdio call to migrate: {updated}"
        );
    }

    #[test]
    fn apply_thread_params_threads_harness_for_non_stdio_capabilities() {
        let cases = [
            (
                "clock_apply.harn",
                Code::LintAmbientClockBuiltin,
                "let value = now_ms()",
                "harness.clock.now_ms()",
            ),
            (
                "fs_apply.harn",
                Code::LintAmbientFsBuiltin,
                "let value = read_file(\"notes.txt\")",
                "harness.fs.read_text(\"notes.txt\")",
            ),
            (
                "env_apply.harn",
                Code::LintAmbientEnvBuiltin,
                "let value = env_or(\"MODE\", \"dev\")",
                "harness.env.get_or(\"MODE\", \"dev\")",
            ),
            (
                "random_apply.harn",
                Code::LintAmbientRandomBuiltin,
                "let value = random_int(0, 10)",
                "harness.random.gen_range(0, 10)",
            ),
            (
                "net_apply.harn",
                Code::LintAmbientNetBuiltin,
                "let value = http_get(\"https://example.test\")",
                "harness.net.get(\"https://example.test\")",
            ),
        ];

        for (filename, code, ambient_line, migrated_call) in cases {
            let temp = tempfile::TempDir::new().unwrap();
            let script = temp.path().join(filename);
            fs::write(
                &script,
                format!(
                    "fn helper() {{\n  {ambient_line}\n  value\n}}\n\nfn main(harness: Harness) {{\n  helper()\n}}\n"
                ),
            )
            .unwrap();

            let result = apply_repairs_with_options(
                &script,
                RepairSafety::SurfaceChanging,
                false,
                FixOptions {
                    harness_threading: HarnessThreadingMode::ThreadParams,
                },
            )
            .unwrap();
            assert!(
                result.applied.iter().any(|repair| {
                    repair.diagnostic_code == code.to_string()
                        && repair.repair_id.starts_with("bindings/thread-harness")
                }),
                "{filename}: {result:#?}"
            );

            let updated = fs::read_to_string(&script).unwrap();
            assert!(
                updated.contains("fn helper(harness: Harness)"),
                "{filename}: expected helper to gain a harness parameter: {updated}"
            );
            assert!(
                updated.contains("helper(harness)"),
                "{filename}: expected main to thread harness into helper: {updated}"
            );
            assert!(
                updated.contains(migrated_call),
                "{filename}: expected ambient call to migrate to {migrated_call}: {updated}"
            );
        }
    }

    #[test]
    fn apply_scope_local_rewrites_ambient_calls_inside_pipeline() {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join("pipeline_direct.harn");
        fs::write(
            &script,
            "pipeline default() {\n  println(\"hi\")\n  let home = env_or(\"HOME\", \"\")\n}\n",
        )
        .unwrap();

        let result = apply_repairs(&script, RepairSafety::ScopeLocal, false).unwrap();
        assert!(
            result.applied.iter().any(|repair| {
                repair.diagnostic_code == Code::LintAmbientStdioBuiltin.to_string()
                    && repair.repair_id == "bindings/thread-harness"
            }),
            "{result:#?}"
        );
        assert!(
            result.applied.iter().any(|repair| {
                repair.diagnostic_code == Code::LintAmbientEnvBuiltin.to_string()
                    && repair.repair_id == "bindings/thread-harness-env"
            }),
            "{result:#?}"
        );

        let updated = fs::read_to_string(&script).unwrap();
        assert!(
            updated.contains("pipeline default()"),
            "pipeline signature should remain stable: {updated}"
        );
        assert!(
            updated.contains("harness.stdio.println(\"hi\")"),
            "expected stdio call to use the pipeline harness global: {updated}"
        );
        assert!(
            updated.contains("harness.env.get_or(\"HOME\", \"\")"),
            "expected env call to use the pipeline harness global: {updated}"
        );
    }

    #[test]
    fn apply_thread_params_threads_harness_from_pipeline_to_helper() {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join("pipeline_helper.harn");
        fs::write(
            &script,
            "pub fn helper() {\n  println(\"hi\")\n}\n\npipeline default() {\n  helper()\n}\n",
        )
        .unwrap();

        let result = apply_repairs_with_options(
            &script,
            RepairSafety::SurfaceChanging,
            false,
            FixOptions {
                harness_threading: HarnessThreadingMode::ThreadParams,
            },
        )
        .unwrap();
        assert!(
            result.applied.iter().any(|repair| {
                repair.diagnostic_code == Code::LintAmbientStdioBuiltin.to_string()
                    && repair.repair_id == "bindings/thread-harness-needs-param"
            }),
            "{result:#?}"
        );

        let updated = fs::read_to_string(&script).unwrap();
        assert!(
            updated.contains("fn helper(harness: Harness)"),
            "expected helper to gain a harness parameter: {updated}"
        );
        assert!(
            updated.contains("helper(harness)"),
            "expected pipeline to pass its harness global into helper: {updated}"
        );
        assert!(
            updated.contains("harness.stdio.println(\"hi\")"),
            "expected ambient stdio call to migrate: {updated}"
        );
    }

    #[test]
    fn apply_scope_local_preserves_stdlib_public_signature_with_global_harness() {
        let temp = tempfile::TempDir::new().unwrap();
        let stdlib_dir = temp.path().join("crates/harn-stdlib/src/stdlib");
        fs::create_dir_all(&stdlib_dir).unwrap();
        let script = stdlib_dir.join("public_helper.harn");
        fs::write(
            &script,
            "/**\n * Public API.\n *\n * @effects: []\n * @errors: []\n */\npub fn helper(path: string) {\n  return read_file(path)\n}\n\npipeline default() {\n  helper(\"notes.txt\")\n}\n",
        )
        .unwrap();

        let result = apply_repairs(&script, RepairSafety::ScopeLocal, false).unwrap();
        assert!(
            result.applied.iter().any(|repair| {
                repair.diagnostic_code == Code::LintAmbientFsBuiltin.to_string()
                    && repair.repair_id == "bindings/use-enclosing-harness-global"
            }),
            "{result:#?}"
        );

        let updated = fs::read_to_string(&script).unwrap();
        assert!(
            updated.contains("pub fn helper(path: string)"),
            "public signature should remain stable: {updated}"
        );
        assert!(
            updated.contains("return harness.fs.read_text(path)"),
            "public function internals should use the VM harness global: {updated}"
        );
        assert!(
            updated.contains("helper(\"notes.txt\")"),
            "callers should not receive an inserted harness argument: {updated}"
        );
    }

    #[test]
    fn apply_default_preserves_non_stdlib_public_signature_with_global_harness() {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join("public_calls_private.harn");
        fs::write(
            &script,
            "/** Public API. */\npub fn load(path: string) {\n  return load_inner(path)\n}\n\nfn load_inner(path: string) {\n  return read_file(path)\n}\n\npipeline default() {\n  load(\"notes.txt\")\n}\n",
        )
        .unwrap();

        let result = apply_repairs(&script, RepairSafety::ScopeLocal, false).unwrap();
        assert!(
            result.applied.iter().any(|repair| {
                repair.diagnostic_code == Code::LintAmbientFsBuiltin.to_string()
                    && repair.repair_id == "bindings/use-enclosing-harness-global"
            }),
            "{result:#?}"
        );

        let updated = fs::read_to_string(&script).unwrap();
        assert!(
            updated.contains("pub fn load(path: string)"),
            "public signature should remain stable: {updated}"
        );
        assert!(
            updated.contains("fn load_inner(path: string)"),
            "private helper signature should remain stable in local-global mode: {updated}"
        );
        assert!(
            updated.contains("return harness.fs.read_text(path)"),
            "private helper should use the VM harness global: {updated}"
        );
        assert!(
            updated.contains("load(\"notes.txt\")"),
            "callers should not receive an inserted harness argument: {updated}"
        );
    }

    #[test]
    fn apply_surface_changing_threads_non_stdlib_public_api() {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join("public_calls_private.harn");
        fs::write(
            &script,
            "/** Public API. */\npub fn load(path: string) {\n  return load_inner(path)\n}\n\nfn load_inner(path: string) {\n  return read_file(path)\n}\n\npipeline default() {\n  load(\"notes.txt\")\n}\n",
        )
        .unwrap();

        let result = apply_repairs_with_options(
            &script,
            RepairSafety::SurfaceChanging,
            false,
            FixOptions {
                harness_threading: HarnessThreadingMode::ThreadParams,
            },
        )
        .unwrap();
        assert!(
            result.applied.iter().any(|repair| {
                repair.diagnostic_code == Code::LintAmbientFsBuiltin.to_string()
                    && repair.repair_id == "bindings/thread-harness-needs-param"
            }),
            "{result:#?}"
        );

        let updated = fs::read_to_string(&script).unwrap();
        assert!(
            updated.contains("pub fn load(harness: Harness, path: string)"),
            "non-stdlib public API should gain an explicit harness parameter: {updated}"
        );
        assert!(
            updated.contains("return load_inner(harness, path)"),
            "public caller should thread its explicit harness parameter: {updated}"
        );
        assert!(
            updated.contains("fn load_inner(harness: Harness, path: string)"),
            "private helper should receive an explicit harness: {updated}"
        );
        assert!(
            updated.contains("return harness.fs.read_text(path)"),
            "private helper should migrate ambient fs call: {updated}"
        );
        assert!(
            updated.contains("load(harness, \"notes.txt\")"),
            "pipeline caller should pass the runtime harness into the public API: {updated}"
        );
    }

    #[test]
    fn apply_dedupes_shared_stdio_threading_edits() {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join("stdio_shared.harn");
        fs::write(
            &script,
            "pub fn leaf_a() {\n  println(\"a\")\n}\n\npub fn leaf_b() {\n  println(\"b\")\n}\n\npub fn middle() {\n  leaf_a()\n  leaf_b()\n}\n\nfn main(harness: Harness) {\n  middle()\n}\n",
        )
        .unwrap();

        let result = apply_repairs_with_options(
            &script,
            RepairSafety::SurfaceChanging,
            false,
            FixOptions {
                harness_threading: HarnessThreadingMode::ThreadParams,
            },
        )
        .unwrap();
        assert!(
            result.applied.iter().any(|repair| {
                repair.diagnostic_code == Code::LintAmbientStdioBuiltin.to_string()
                    && repair.repair_id == "bindings/thread-harness-needs-param"
            }),
            "{result:#?}"
        );

        let updated = fs::read_to_string(&script).unwrap();
        assert!(
            updated.contains("fn middle(harness: Harness)"),
            "expected middle to receive exactly one harness parameter: {updated}"
        );
        assert!(
            !updated.contains("fn middle(harness: Harness, harness: Harness"),
            "shared threading edits should not duplicate params: {updated}"
        );
        assert!(
            updated.contains("leaf_a(harness)") && updated.contains("leaf_b(harness)"),
            "expected both leaf calls to receive harness: {updated}"
        );
    }

    #[test]
    fn plan_uses_underscore_harness_when_harness_name_is_taken() {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join("stdio_taken_name.harn");
        fs::write(
            &script,
            "fn helper(harness: string) {\n  println(harness)\n}\n",
        )
        .unwrap();

        let plan = build_plan(&script, None).unwrap();
        let repair = plan
            .repairs
            .iter()
            .find(|repair| repair.diagnostic_code == Code::LintAmbientStdioBuiltin.to_string())
            .expect("ambient stdio repair should be present");

        let replacements = repair
            .edits
            .iter()
            .map(|edit| edit.replacement.as_str())
            .collect::<Vec<_>>();
        assert!(
            replacements.contains(&"_harness: Harness, "),
            "expected inserted capability parameter to avoid duplicate `harness`: {replacements:?}"
        );
        assert!(
            replacements.contains(&"_harness.stdio.println"),
            "expected call rewrite to use the inserted capability parameter: {replacements:?}"
        );
    }
}
