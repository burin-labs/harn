//! `harn fix`: propose or apply repair-bearing diagnostics.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

use harn_lexer::{FixEdit, Span};
use harn_lint::LintSeverity;
use harn_parser::{
    diagnostic::harness_stdio_replacement, peel_attributes, visit, DiagnosticCode as Code,
    DiagnosticSeverity, Node, Repair, RepairSafety, SNode, TypeChecker, TypeExpr, TypedParam,
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
    params: Vec<TypedParam>,
    is_pub: bool,
}

#[derive(Debug, Clone)]
struct CallSite {
    caller: String,
    callee: String,
    span: Span,
    arg_count: usize,
}

#[derive(Debug, Default)]
struct FileCallableGraph {
    callables: HashMap<String, CallableInfo>,
    callsites_by_callee: HashMap<String, Vec<CallSite>>,
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
            let mut deduped = edits.clone();
            dedupe_fix_edit_wires(&mut deduped);
            apply_file_edits(Path::new(path), &deduped)?;
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

fn dedupe_fix_edit_wires(edits: &mut Vec<FixEditWire>) {
    edits.sort_by(|left, right| {
        left.span
            .start
            .cmp(&right.span.start)
            .then(left.span.end.cmp(&right.span.end))
            .then(left.replacement.cmp(&right.replacement))
    });
    edits.dedup_by(|left, right| {
        left.span.start == right.span.start
            && left.span.end == right.span.end
            && left.replacement == right.replacement
    });
}

fn build_file_callable_graph(program: &[SNode]) -> FileCallableGraph {
    let mut graph = FileCallableGraph::default();
    for node in program {
        let (_, inner) = peel_attributes(node);
        let (name, params, body, is_pub) = match &inner.node {
            Node::FnDecl {
                name,
                params,
                body,
                is_pub,
                ..
            } => (name.clone(), params.clone(), body, *is_pub),
            Node::ToolDecl {
                name,
                params,
                body,
                is_pub,
                ..
            } => (name.clone(), params.clone(), body, *is_pub),
            _ => continue,
        };
        graph.callables.insert(
            name.clone(),
            CallableInfo {
                name: name.clone(),
                span: inner.span,
                params,
                is_pub,
            },
        );
        let mut local_calls = Vec::new();
        for stmt in body {
            visit::walk_node(stmt, &mut |child| {
                if let Node::FunctionCall { name, args, .. } = &child.node {
                    local_calls.push(CallSite {
                        caller: name.clone(), // overwritten below if callee is local
                        callee: name.clone(),
                        span: child.span,
                        arg_count: args.len(),
                    });
                }
            });
        }
        for mut call in local_calls {
            if graph.callables.contains_key(&call.callee)
                || callable_declared_in_program(program, &call.callee)
            {
                call.caller = name.clone();
                graph
                    .callsites_by_callee
                    .entry(call.callee.clone())
                    .or_default()
                    .push(call);
            }
        }
    }
    graph
}

fn callable_declared_in_program(program: &[SNode], target: &str) -> bool {
    program.iter().any(|node| {
        let (_, inner) = peel_attributes(node);
        matches!(
            &inner.node,
            Node::FnDecl { name, .. } | Node::ToolDecl { name, .. } if name == target
        )
    })
}

fn synthesize_stdio_threaded_repair(
    source: &str,
    _program: &[SNode],
    graph: &FileCallableGraph,
    diag_span: Span,
) -> Option<(Repair, Vec<FixEdit>)> {
    let owner = owning_callable(graph, diag_span)?;
    let callable = graph.callables.get(&owner)?;
    let ambient_name_span = function_call_name_span(source, diag_span)?;
    let ambient_name = source.get(ambient_name_span.start..ambient_name_span.end)?;
    let replacement = harness_stdio_replacement(ambient_name)?;
    let local_harness_name =
        callable_harness_name(&callable.params).unwrap_or_else(|| "harness".to_string());

    let mut edits = vec![replace_identifier_fix(ambient_name_span, replacement)];
    let mut needs_param = BTreeSet::new();
    let mut seen = HashSet::new();
    let mut queue = VecDeque::from([owner.clone()]);
    let mut safety = if callable.is_pub || callable.name == "main" {
        RepairSafety::SurfaceChanging
    } else {
        RepairSafety::ScopeLocal
    };

    while let Some(current) = queue.pop_front() {
        if !seen.insert(current.clone()) {
            continue;
        }
        let current_callable = match graph.callables.get(&current) {
            Some(callable) => callable,
            None => continue,
        };
        if !callable_has_harness_param(&current_callable.params) {
            needs_param.insert(current.clone());
        }
        let mut has_local_caller = false;
        for callsite in graph
            .callsites_by_callee
            .get(&current)
            .into_iter()
            .flatten()
            .cloned()
        {
            has_local_caller = true;
            let caller = match graph.callables.get(&callsite.caller) {
                Some(caller) => caller,
                None => continue,
            };
            let caller_harness_name =
                callable_harness_name(&caller.params).unwrap_or_else(|| "harness".to_string());
            edits.push(add_call_argument_edit(
                source,
                callsite.span,
                callsite.arg_count,
                &caller_harness_name,
            )?);
            if (caller.is_pub || caller.name == "main")
                && !callable_has_harness_param(&caller.params)
            {
                safety = RepairSafety::SurfaceChanging;
            }
            if !callable_has_harness_param(&caller.params) {
                queue.push_back(caller.name.clone());
            }
        }
        if !has_local_caller && !callable_has_harness_param(&current_callable.params) {
            safety = RepairSafety::SurfaceChanging;
        }
    }

    for callable_name in needs_param {
        let callable = graph.callables.get(&callable_name)?;
        let param_name = if callable.name == owner {
            local_harness_name.clone()
        } else {
            "harness".to_string()
        };
        edits.push(add_harness_param_edit(source, callable.span, &param_name)?);
    }
    dedupe_fix_edits(&mut edits);

    let repair_id = if safety == RepairSafety::SurfaceChanging {
        "bindings/thread-harness-needs-param"
    } else {
        "bindings/thread-harness-stdio"
    };
    let template = harn_parser::REPAIR_REGISTRY
        .iter()
        .copied()
        .find(|template| template.id == repair_id)?;
    let mut repair = Repair::from_template(template);
    repair.safety = safety;
    Some((repair, edits))
}

fn owning_callable(graph: &FileCallableGraph, span: Span) -> Option<String> {
    graph
        .callables
        .values()
        .filter(|callable| callable.span.start <= span.start && callable.span.end >= span.end)
        .min_by_key(|callable| callable.span.end - callable.span.start)
        .map(|callable| callable.name.clone())
}

fn callable_has_harness_param(params: &[TypedParam]) -> bool {
    callable_harness_name(params).is_some()
}

fn callable_harness_name(params: &[TypedParam]) -> Option<String> {
    params
        .iter()
        .find(|param| is_harness_param(param))
        .map(|param| param.name.clone())
}

fn is_harness_param(param: &TypedParam) -> bool {
    matches!(param.type_expr.as_ref(), Some(TypeExpr::Named(name)) if name == "Harness")
        && matches!(param.name.as_str(), "harness" | "_harness")
}

fn add_harness_param_edit(source: &str, callable_span: Span, param_name: &str) -> Option<FixEdit> {
    let open = source
        .get(callable_span.start..callable_span.end)?
        .find('(')?
        + callable_span.start;
    let close = find_matching_paren(source, open)?;
    let has_params = !source.get((open + 1)..close)?.trim().is_empty();
    let insert = if has_params {
        format!("{param_name}: Harness, ")
    } else {
        format!("{param_name}: Harness")
    };
    Some(FixEdit {
        span: zero_length_span(source, open + 1),
        replacement: insert,
    })
}

fn add_call_argument_edit(
    source: &str,
    call_span: Span,
    arg_count: usize,
    arg_name: &str,
) -> Option<FixEdit> {
    let open = source.get(call_span.start..call_span.end)?.find('(')? + call_span.start;
    let insert = if arg_count == 0 {
        arg_name.to_string()
    } else {
        format!("{arg_name}, ")
    };
    Some(FixEdit {
        span: zero_length_span(source, open + 1),
        replacement: insert,
    })
}

fn replace_identifier_fix(span: Span, replacement: &str) -> FixEdit {
    FixEdit {
        span,
        replacement: replacement.to_string(),
    }
}

fn function_call_name_span(source: &str, call_span: Span) -> Option<Span> {
    let call_source = source.get(call_span.start..call_span.end)?;
    let open = call_source.find('(')?;
    Some(Span::with_offsets(
        call_span.start,
        call_span.start + open,
        call_span.line,
        call_span.column,
    ))
}

fn dedupe_fix_edits(edits: &mut Vec<FixEdit>) {
    edits.sort_by(|left, right| {
        left.span
            .start
            .cmp(&right.span.start)
            .then(left.span.end.cmp(&right.span.end))
            .then(left.replacement.cmp(&right.replacement))
    });
    edits.dedup_by(|left, right| {
        left.span.start == right.span.start
            && left.span.end == right.span.end
            && left.replacement == right.replacement
    });
}

fn find_matching_paren(source: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, ch) in source.get(open..)?.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn zero_length_span(source: &str, offset: usize) -> Span {
    let prefix = source.get(..offset).unwrap_or("");
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = prefix
        .rsplit('\n')
        .next()
        .map(|line_text| line_text.chars().count() + 1)
        .unwrap_or(1);
    Span::with_offsets(offset, offset, line, column)
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
    let callable_graph = build_file_callable_graph(&program);
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
        let Some(mut repair) = diag.repair() else {
            continue;
        };
        let mut edits = diag.fix.clone().unwrap_or_default();
        if diag.code == Code::LintAmbientStdioBuiltin && edits.is_empty() {
            if let Some((synthesized_repair, synthesized_edits)) =
                synthesize_stdio_threaded_repair(&source, &program, &callable_graph, diag.span)
            {
                repair = synthesized_repair;
                edits = synthesized_edits;
            }
        }
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

fn spans_overlap(left: Span, right: Span) -> bool {
    left.start < right.end && left.end > right.start
}

fn edits_conflict(left: &FixEdit, right: &FixEdit) -> bool {
    if left.span.start == right.span.start
        && left.span.end == right.span.end
        && left.replacement == right.replacement
    {
        return false;
    }
    spans_overlap(left.span, right.span)
        || (left.span.start == left.span.end
            && right.span.start == right.span.end
            && left.span.start == right.span.start
            && left.replacement != right.replacement)
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
    fn plan_reports_surface_changing_stdio_migration_when_harness_is_missing() {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join("stdio_demo.harn");
        fs::write(&script, "fn helper() {\n  println(\"hi\")\n}\n").unwrap();

        let plan = build_plan(&script, Some(RepairSafety::SurfaceChanging)).unwrap();
        let repair = plan
            .repairs
            .iter()
            .find(|repair| repair.diagnostic_code == Code::LintAmbientStdioBuiltin.to_string())
            .expect("stdio repair present");

        assert_eq!(repair.repair.id, "bindings/thread-harness-needs-param");
        assert_eq!(repair.repair.safety, "surface-changing");
        assert!(!repair.edits.is_empty(), "{repair:#?}");
    }

    #[test]
    fn apply_threads_harness_through_same_file_callers_for_stdio() {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join("stdio_demo.harn");
        fs::write(
            &script,
            concat!(
                "fn helper() {\n",
                "  println(\"hi\")\n",
                "}\n",
                "\n",
                "fn wrapper() {\n",
                "  helper()\n",
                "}\n",
                "\n",
                "fn main(harness: Harness) {\n",
                "  wrapper()\n",
                "}\n",
            ),
        )
        .unwrap();

        let result = apply_repairs(&script, RepairSafety::ScopeLocal, false).unwrap();

        assert_eq!(result.applied.len(), 1, "{result:#?}");
        assert_eq!(result.skipped.len(), 1, "{result:#?}");
        assert_eq!(result.skipped[0].reason, "no_edits", "{result:#?}");
        let updated = fs::read_to_string(&script).unwrap();
        assert!(updated.contains("fn helper(harness: Harness)"), "{updated}");
        assert!(
            updated.contains("fn wrapper(harness: Harness)"),
            "{updated}"
        );
        assert!(
            updated.contains("harness.stdio.println(\"hi\")"),
            "{updated}"
        );
        assert!(updated.contains("helper(harness)"), "{updated}");
        assert!(updated.contains("wrapper(harness)"), "{updated}");
    }

    #[test]
    fn plan_uses_scope_local_stdio_repair_when_harness_is_reachable() {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join("stdio_demo.harn");
        fs::write(
            &script,
            concat!(
                "fn helper() {\n",
                "  println(\"hi\")\n",
                "}\n",
                "\n",
                "fn wrapper() {\n",
                "  helper()\n",
                "}\n",
                "\n",
                "fn main(harness: Harness) {\n",
                "  wrapper()\n",
                "}\n",
            ),
        )
        .unwrap();

        let plan = build_plan(&script, Some(RepairSafety::ScopeLocal)).unwrap();
        let repair = plan
            .repairs
            .iter()
            .find(|repair| repair.diagnostic_code == Code::LintAmbientStdioBuiltin.to_string())
            .expect("stdio repair present");

        assert_eq!(repair.repair.id, "bindings/thread-harness-stdio");
        assert_eq!(repair.repair.safety, "scope-local");
        assert!(repair.applies_cleanly, "{plan:#?}");
    }

    #[test]
    fn plan_dedupes_shared_harness_threading_edits() {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join("stdio_demo.harn");
        fs::write(
            &script,
            concat!(
                "fn helper() {\n",
                "  println(\"one\")\n",
                "  println(\"two\")\n",
                "}\n",
                "\n",
                "fn main(harness: Harness) {\n",
                "  helper()\n",
                "}\n",
            ),
        )
        .unwrap();

        let plan = build_plan(&script, Some(RepairSafety::ScopeLocal)).unwrap();
        let repairs = plan
            .repairs
            .iter()
            .filter(|repair| repair.diagnostic_code == Code::LintAmbientStdioBuiltin.to_string())
            .collect::<Vec<_>>();

        assert_eq!(repairs.len(), 2, "{plan:#?}");
        assert!(
            repairs.iter().all(|repair| repair.applies_cleanly),
            "{plan:#?}"
        );
    }
}
