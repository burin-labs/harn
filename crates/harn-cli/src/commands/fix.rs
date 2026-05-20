//! `harn fix`: propose or apply repair-bearing diagnostics.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use harn_lexer::{FixEdit, Span};
use harn_lint::LintSeverity;
use harn_parser::{
    visit, DiagnosticCode as Code, DiagnosticSeverity, Node, Repair, RepairSafety, SNode,
    TypeChecker, TypeExpr, TypedParam,
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

#[derive(Debug, Clone)]
struct CallableInfo {
    name: String,
    span: Span,
    is_pub: bool,
    insert_offset: usize,
    has_params: bool,
    param_names: BTreeSet<String>,
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
            let edits = dedupe_wire_edits(edits);
            apply_file_edits(Path::new(path), &edits)?;
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
        let Some((repair, edits)) = lint_candidate_repair(diag, &source, &program) else {
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
            edits,
        });
    }

    collect_preflight_candidates(file, &source, &program, &config, safety_ceiling, out);
}

fn lint_candidate_repair(
    diag: &harn_lint::LintDiagnostic,
    source: &str,
    program: &[SNode],
) -> Option<(Repair, Vec<FixEdit>)> {
    if ambient_capability_handle(diag.code).is_some() {
        return synthesize_ambient_capability_repair(diag, source, program);
    }
    let repair = diag.repair()?;
    Some((repair, diag.fix.clone().unwrap_or_default()))
}

fn synthesize_ambient_capability_repair(
    diag: &harn_lint::LintDiagnostic,
    source: &str,
    program: &[SNode],
) -> Option<(Repair, Vec<FixEdit>)> {
    ambient_capability_handle(diag.code)?;
    let infos = collect_callable_infos(program, source);
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
    let owner_uses_global_harness = should_use_global_harness(owner);
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

    if owner.harness_binding.is_some() || owner_uses_global_harness {
        return Some((Repair::from_template(diag.code.repair_template()?), edits));
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
                None if should_use_global_harness(caller) => "harness",
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
    ))
}

fn should_use_global_harness(info: &CallableInfo) -> bool {
    // Public Harn APIs need to keep their source-level signature stable while
    // their internals migrate off ambient builtins. The VM installs `harness`
    // as a global before execution, so using that binding is compatible with
    // existing callers. Pipeline bodies are modeled separately with an
    // explicit harness binding because their runtime entry parameters are not
    // invocation parameters.
    info.is_pub && !info.param_names.contains("harness")
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

fn collect_callable_infos(program: &[SNode], source: &str) -> Vec<CallableInfo> {
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
                is_pub,
                ..
            }
            | Node::ToolDecl {
                name,
                params,
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
                infos.push(CallableInfo {
                    name: name.clone(),
                    span: inner.span,
                    is_pub: *is_pub,
                    insert_offset,
                    has_params: has_params || !params.is_empty(),
                    param_names: params.iter().map(|param| param.name.clone()).collect(),
                    harness_binding: harness_param_name(params).map(str::to_string),
                    can_add_harness_param: true,
                    calls,
                    ambient_capability_calls,
                });
            }
            Node::Pipeline {
                name,
                params,
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
                infos.push(CallableInfo {
                    name: name.clone(),
                    span: inner.span,
                    is_pub: *is_pub,
                    insert_offset,
                    has_params: has_params || !params.is_empty(),
                    param_names: params.iter().cloned().collect(),
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
) -> BTreeSet<usize> {
    let mut needed = BTreeSet::from([owner_idx]);
    let mut changed = true;
    while changed {
        changed = false;
        let snapshot = needed.iter().copied().collect::<Vec<_>>();
        for callee_idx in snapshot {
            for &(caller_idx, _) in &reverse_callers[callee_idx] {
                if infos[caller_idx].harness_binding.is_none()
                    && !should_use_global_harness(&infos[caller_idx])
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
        info.is_pub || info.name == "main" || reverse_callers[idx].is_empty()
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
    if !info.param_names.contains("harness") {
        return Some("harness");
    }
    if !info.param_names.contains("_harness") {
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

    #[test]
    fn plan_threads_existing_harness_for_stdio_repairs() {
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

        assert_eq!(repair.repair.id, "bindings/thread-harness");
        assert_eq!(repair.repair.safety, "scope-local");
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
            replacements.contains(&"harness: Harness"),
            "expected helper parameter insertion in edits: {replacements:?}"
        );
        assert!(
            replacements.contains(&"harness"),
            "expected caller argument threading in edits: {replacements:?}"
        );
    }

    #[test]
    fn plan_marks_stdio_repairs_surface_changing_when_harness_is_unreachable() {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join("stdio_needs_param.harn");
        fs::write(&script, "fn helper() {\n  println(\"hi\")\n}\n").unwrap();

        let plan = build_plan(&script, None).unwrap();
        let repair = plan
            .repairs
            .iter()
            .find(|repair| repair.diagnostic_code == Code::LintAmbientStdioBuiltin.to_string())
            .expect("ambient stdio repair should be present");

        assert_eq!(repair.repair.id, "bindings/thread-harness-needs-param");
        assert_eq!(repair.repair.safety, "surface-changing");
    }

    #[test]
    fn apply_scope_local_threads_harness_for_stdio_migration() {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join("stdio_apply.harn");
        fs::write(
            &script,
            "fn helper() {\n  println(\"hi\")\n}\n\nfn main(harness: Harness) {\n  helper()\n}\n",
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
    fn apply_scope_local_threads_harness_for_non_stdio_capabilities() {
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

            let result = apply_repairs(&script, RepairSafety::ScopeLocal, false).unwrap();
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
    fn apply_scope_local_threads_harness_from_pipeline_to_helper() {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join("pipeline_helper.harn");
        fs::write(
            &script,
            "fn helper() {\n  println(\"hi\")\n}\n\npipeline default() {\n  helper()\n}\n",
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
    fn apply_scope_local_preserves_public_signature_with_global_harness() {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join("public_helper.harn");
        fs::write(
            &script,
            "/** Public API. */\npub fn helper(path: string) {\n  return read_file(path)\n}\n\npipeline default() {\n  helper(\"notes.txt\")\n}\n",
        )
        .unwrap();

        let result = apply_repairs(&script, RepairSafety::ScopeLocal, false).unwrap();
        assert!(
            result.applied.iter().any(|repair| {
                repair.diagnostic_code == Code::LintAmbientFsBuiltin.to_string()
                    && repair.repair_id == "bindings/thread-harness-fs"
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
    fn apply_scope_local_threads_private_helper_from_public_api() {
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
                    && repair.repair_id == "bindings/thread-harness-fs"
            }),
            "{result:#?}"
        );

        let updated = fs::read_to_string(&script).unwrap();
        assert!(
            updated.contains("pub fn load(path: string)"),
            "public signature should remain stable: {updated}"
        );
        assert!(
            updated.contains("return load_inner(harness, path)"),
            "public caller should thread the VM harness global: {updated}"
        );
        assert!(
            updated.contains("fn load_inner(harness: Harness, path: string)"),
            "private helper should receive an explicit harness: {updated}"
        );
        assert!(
            updated.contains("return harness.fs.read_text(path)"),
            "private helper should migrate ambient fs call: {updated}"
        );
    }

    #[test]
    fn apply_dedupes_shared_stdio_threading_edits() {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join("stdio_shared.harn");
        fs::write(
            &script,
            "fn leaf_a() {\n  println(\"a\")\n}\n\nfn leaf_b() {\n  println(\"b\")\n}\n\nfn middle() {\n  leaf_a()\n  leaf_b()\n}\n\nfn main(harness: Harness) {\n  middle()\n}\n",
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
