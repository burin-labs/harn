use super::*;

#[cfg(test)]
pub(super) fn apply_repairs(
    target: &Path,
    safety_ceiling: RepairSafety,
    dry_run: bool,
) -> Result<ApplyResult, String> {
    apply_repairs_with_options(target, safety_ceiling, dry_run, FixOptions::default())
}

pub(super) fn apply_repairs_with_options(
    target: &Path,
    safety_ceiling: RepairSafety,
    dry_run: bool,
    options: FixOptions,
) -> Result<ApplyResult, String> {
    let mut applied = Vec::new();
    let mut skipped = Vec::new();
    let max_passes = if options.capability_migrations_only && !dry_run {
        CAPABILITY_MIGRATION_MAX_PASSES
    } else {
        1
    };
    let mut converged = false;

    for _ in 0..max_passes {
        let plan = build_plan_with_options(target, None, options)?;
        let mut edits_by_file: BTreeMap<String, Vec<FixEditWire>> = BTreeMap::new();
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

        if dry_run || edits_by_file.is_empty() {
            converged = true;
            break;
        }
        for (path, edits) in &edits_by_file {
            let edits = dedupe_wire_edits(edits);
            apply_file_edits(Path::new(path), &edits)?;
        }
        if !options.capability_migrations_only {
            converged = true;
            break;
        }
    }

    if !converged {
        return Err(format!(
            "capability migration did not converge after {CAPABILITY_MIGRATION_MAX_PASSES} passes"
        ));
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

pub(super) fn repair_path(plan: &RepairPlan, repair: &RepairWire) -> Result<String, String> {
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

        let lint_context = FixLintContext::load(file);
        let options = lint_context.options(file);
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
            count += preflight_diagnostics(file, &source, &program, &config, &module_graph)
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
