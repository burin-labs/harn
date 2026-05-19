//! `harn fix`: propose or apply repair-bearing diagnostics.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use harn_lexer::{FixEdit, Span};
use harn_lint::LintSeverity;
use harn_parser::{
    DiagnosticCode as Code, DiagnosticSeverity, Repair, RepairSafety, SNode, TypeChecker,
};
use serde::Serialize;

use crate::cli::FixArgs;
use crate::package::{self, CheckConfig, PreflightSeverity};
use crate::{commands, parse_source_file};

pub(crate) const FIX_PLAN_SCHEMA_VERSION: u32 = 1;
pub(crate) const FIX_APPLY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RepairPlan {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub path: String,
    pub diagnostics: Vec<DiagnosticWire>,
    pub repairs: Vec<RepairWire>,
    #[serde(rename = "safetyLevels")]
    pub safety_levels: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ApplyResult {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub applied: Vec<AppliedRepairWire>,
    pub skipped: Vec<SkippedRepairWire>,
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
    pub edits: Vec<FixEditWire>,
    pub applies_cleanly: bool,
    pub conflicts_with: Vec<usize>,
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
    edits: Vec<FixEdit>,
}

pub(crate) fn run(args: &FixArgs) -> Result<(), String> {
    if args.apply {
        let safety = args.safety.ok_or_else(|| {
            "`harn fix --apply` requires `--safety <format-only|behavior-preserving|scope-local|surface-changing|capability-changing>`"
                .to_string()
        })?;
        if safety == RepairSafety::NeedsHuman {
            return Err(
                "`harn fix --apply --safety needs-human` is not allowed; use `harn fix --plan --json` to inspect propose-only repairs"
                    .to_string(),
            );
        }
        let result = apply_repairs(&args.path, safety, args.dry_run)?;
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&result)
                    .map_err(|error| format!("failed to serialize apply result: {error}"))?
            );
        } else {
            print_apply_result(&result);
        }
        return Ok(());
    }
    if !args.plan {
        return Err("`harn fix` requires `--plan` or `--apply`".to_string());
    }

    let plan = build_plan(&args.path, args.safety)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&plan)
                .map_err(|error| format!("failed to serialize repair plan: {error}"))?
        );
    } else {
        print_human_plan(&plan);
    }
    Ok(())
}

pub(crate) fn build_plan(
    target: &Path,
    safety_ceiling: Option<RepairSafety>,
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
    let mut candidates = Vec::new();
    for file in &files {
        collect_file_candidates(
            file,
            safety_ceiling,
            &cross_file_imports,
            &module_graph,
            &mut candidates,
        );
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
        safety_levels,
    })
}

pub(crate) fn apply_repairs(
    target: &Path,
    safety_ceiling: RepairSafety,
    dry_run: bool,
) -> Result<ApplyResult, String> {
    let plan = build_plan(target, None)?;
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
            apply_file_edits(Path::new(path), edits)?;
        }
    }

    let post_apply_diagnostics_count = count_remaining_diagnostics(target)?;
    Ok(ApplyResult {
        schema_version: FIX_APPLY_SCHEMA_VERSION,
        applied,
        skipped,
        post_apply_diagnostics_count,
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

fn count_remaining_diagnostics(target: &Path) -> Result<usize, String> {
    if let Err(error) = package::validate_runtime_manifest_extensions(target) {
        return Err(format!("manifest extension validation failed: {error}"));
    }

    let target_string = target.to_string_lossy().into_owned();
    let target_refs = [target_string.as_str()];
    let files = commands::check::collect_harn_targets(&target_refs);
    let module_graph = commands::check::build_module_graph(&files);
    let cross_file_imports = commands::check::collect_cross_file_imports(&module_graph);
    let mut count = 0;

    for file in &files {
        let path_str = file.to_string_lossy().into_owned();
        let (source, program) = parse_source_file(&path_str);
        let mut config = package::load_check_config(Some(file));
        commands::check::apply_harn_lint_config(file, &mut config);

        count += type_check(file, &config, &module_graph, &program, &source)
            .iter()
            .filter(|diag| !harn_lint::type_diagnostic_lint_disabled(diag, &config.disable_rules))
            .count();

        let persona_step_allowlist = commands::check::harn_lint_persona_step_allowlist(file);
        let options = harn_lint::LintOptions {
            file_path: Some(file),
            require_file_header: commands::check::harn_lint_require_file_header(file),
            complexity_threshold: commands::check::harn_lint_complexity_threshold(file),
            persona_step_allowlist: &persona_step_allowlist,
            require_stdlib_metadata: commands::check::path_is_stdlib_source(file),
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

    Ok(count)
}

fn collect_file_candidates(
    file: &Path,
    safety_ceiling: Option<RepairSafety>,
    cross_file_imports: &HashSet<String>,
    module_graph: &harn_modules::ModuleGraph,
    out: &mut Vec<RepairCandidate>,
) {
    let path_str = file.to_string_lossy().into_owned();
    let (source, program) = parse_source_file(&path_str);
    let mut config = package::load_check_config(Some(file));
    commands::check::apply_harn_lint_config(file, &mut config);

    let type_diagnostics = type_check(file, &config, module_graph, &program, &source);
    for diag in &type_diagnostics {
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
            edits: diag.fix.clone().unwrap_or_default(),
        });
    }

    let persona_step_allowlist = commands::check::harn_lint_persona_step_allowlist(file);
    let options = harn_lint::LintOptions {
        file_path: Some(file),
        require_file_header: commands::check::harn_lint_require_file_header(file),
        complexity_threshold: commands::check::harn_lint_complexity_threshold(file),
        persona_step_allowlist: &persona_step_allowlist,
        require_stdlib_metadata: commands::check::path_is_stdlib_source(file),
    };
    let lint_diagnostics = harn_lint::lint_with_module_graph(
        &program,
        &config.disable_rules,
        Some(&source),
        cross_file_imports,
        module_graph,
        file,
        &options,
    );
    for diag in &lint_diagnostics {
        let Some(repair) = diag.repair() else {
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
            edits: diag.fix.clone().unwrap_or_default(),
        });
    }

    collect_preflight_candidates(file, &source, &program, &config, safety_ceiling, out);
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
            edits: Vec::new(),
        });
    }
}

fn type_check(
    path: &Path,
    config: &CheckConfig,
    module_graph: &harn_modules::ModuleGraph,
    program: &[SNode],
    source: &str,
) -> Vec<harn_parser::TypeDiagnostic> {
    let mut checker = TypeChecker::with_strict_types(config.strict_types);
    if let Some(imported) = module_graph.imported_names_for_file(path) {
        checker = checker.with_imported_names(imported);
    }
    if let Some(imported) = module_graph.imported_type_declarations_for_file(path) {
        checker = checker.with_imported_type_decls(imported);
    }
    if let Some(imported) = module_graph.imported_callable_declarations_for_file(path) {
        checker = checker.with_imported_callable_decls(imported);
    }
    checker.check_with_source(program, source)
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
            .any(|right_edit| spans_overlap(left_edit.span, right_edit.span))
    })
}

fn spans_overlap(left: Span, right: Span) -> bool {
    left.start < right.end && left.end > right.start
}

fn print_human_plan(plan: &RepairPlan) {
    if plan.repairs.is_empty() {
        println!("{}: no repairable diagnostics found", plan.path);
        return;
    }
    println!(
        "{}: {} repairable diagnostic(s)",
        plan.path,
        plan.repairs.len()
    );
    println!("idx  code          safety               edits  clean  repair");
    for repair in &plan.repairs {
        let clean = if repair.applies_cleanly { "yes" } else { "no" };
        println!(
            "{:<4} {:<13} {:<20} {:<5} {:<5} {}",
            repair.diagnostic_index,
            repair.diagnostic_code,
            repair.repair.safety,
            repair.edits.len(),
            clean,
            repair.repair.id
        );
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
            json: false,
            path: PathBuf::from("repair_demo.harn"),
        };

        let error = run(&args).unwrap_err();
        assert!(
            error.contains("needs-human") && error.contains("--plan --json"),
            "unexpected error: {error}"
        );
    }
}
