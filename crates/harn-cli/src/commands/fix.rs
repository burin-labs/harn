//! `harn fix --plan`: propose repair-bearing diagnostics without edits.

use std::collections::{BTreeSet, HashSet};
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
        return Err("`harn fix --apply` is tracked by #1752; use `--plan` for now".to_string());
    }
    if !args.plan {
        return Err("`harn fix` requires `--plan` until apply mode lands".to_string());
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

fn severity_label(severity: DiagnosticSeverity) -> &'static str {
    match severity {
        DiagnosticSeverity::Error => "error",
        DiagnosticSeverity::Warning => "warning",
    }
}

fn lint_severity_label(severity: LintSeverity) -> &'static str {
    match severity {
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
}
