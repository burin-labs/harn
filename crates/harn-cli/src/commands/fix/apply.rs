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
    let mut original_files = BTreeMap::new();
    let result = apply_repairs_with_options_inner(
        target,
        safety_ceiling,
        dry_run,
        options,
        &mut original_files,
    );
    finish_with_rollback(result, dry_run, &original_files)
}

fn apply_repairs_with_options_inner(
    target: &Path,
    safety_ceiling: RepairSafety,
    dry_run: bool,
    options: FixOptions,
    original_files: &mut BTreeMap<String, String>,
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
        let retired_testing_prerequisite = plan.repairs.iter().any(|repair| {
            repair.repair.id == "imports/remove-retired-testing-helper"
                && repair.applies_cleanly
                && !repair.edits.is_empty()
        });
        let mut edits_by_file: BTreeMap<String, Vec<FixEditWire>> = BTreeMap::new();
        for repair in &plan.repairs {
            if retired_testing_prerequisite
                && repair.repair.id != "imports/remove-retired-testing-helper"
            {
                // Removing a retired import changes every later byte offset in
                // the file. Rebuild the complete program plan from that new
                // source on the next pass instead of mixing source versions.
                continue;
            }
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
                .entry(edit_group_key(&path))
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
        for path in edits_by_file.keys() {
            if !original_files.contains_key(path) {
                let source = std::fs::read_to_string(path)
                    .map_err(|error| format!("failed to snapshot {path} before repair: {error}"))?;
                original_files.insert(path.clone(), source);
            }
        }
        if options.capability_migrations_only {
            let rendered_files = render_capability_migration_pass(&edits_by_file)?;
            for (path, rendered) in rendered_files {
                std::fs::write(&path, rendered)
                    .map_err(|error| format!("failed to write {path}: {error}"))?;
            }
        } else {
            for (path, edits) in &edits_by_file {
                let edits = dedupe_wire_edits(edits);
                apply_file_edits(Path::new(path), &edits)?;
            }
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

pub(super) fn render_capability_migration_pass(
    edits_by_file: &BTreeMap<String, Vec<FixEditWire>>,
) -> Result<BTreeMap<String, String>, String> {
    let mut rendered_files = BTreeMap::new();
    for (path, edits) in edits_by_file {
        let edits = dedupe_wire_edits(edits);
        let path_ref = Path::new(path);
        let candidate = (|| {
            let edited = edited_source(path_ref, &edits)?;
            let candidate = format_capability_candidate(path_ref, &edited)?;
            harn_parser::parse_source(&candidate)
                .map_err(|errors| format!("invalid Harn syntax: {errors:?}"))?;
            Ok::<_, String>(candidate)
        })()
        .map_err(|error| {
            format!(
                "capability migration rejected the candidate for {path}; no files from this pass were written: {error}"
            )
        })?;
        rendered_files.insert(path.clone(), candidate);
    }
    Ok(rendered_files)
}

pub(super) fn restore_original_files(
    original_files: &BTreeMap<String, String>,
) -> Result<(), String> {
    let mut failures = Vec::new();
    for (path, source) in original_files {
        if let Err(error) = std::fs::write(path, source) {
            failures.push(format!("{path}: {error}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

pub(super) fn finish_with_rollback<T>(
    result: Result<T, String>,
    dry_run: bool,
    original_files: &BTreeMap<String, String>,
) -> Result<T, String> {
    match result {
        Ok(value) => Ok(value),
        Err(error) if dry_run => Err(error),
        Err(error) => match restore_original_files(original_files) {
            Ok(()) => Err(error),
            Err(restore_error) => Err(format!(
                "{error}; additionally failed to roll back the `harn fix` transaction: {restore_error}"
            )),
        },
    }
}

#[cfg(test)]
pub(super) fn format_edited_files(paths: &BTreeSet<String>) -> Result<(), String> {
    for path in paths {
        let source = std::fs::read_to_string(path)
            .map_err(|error| format!("failed to read {path} before formatting: {error}"))?;
        let formatted = format_capability_candidate(Path::new(path), &source)?;
        if source != formatted {
            std::fs::write(path, formatted).map_err(|error| {
                format!("failed to write formatted migration output {path}: {error}")
            })?;
        }
    }
    Ok(())
}

/// Group edits by the file they land in, not by how a diagnostic spelled it.
///
/// Diagnostics reach the plan from two passes that name the same file
/// differently: the per-file lint pass reports `./src/workflow.harn` while the
/// whole-program capability pass reports an absolute path. Keying edits on the
/// raw string therefore splits one file into two groups, and because every
/// group re-reads the file from disk, the second group applies spans computed
/// against source the first group already rewrote. `.` sorts before `/`, so the
/// relative group landed first and the absolute group's offsets were stale by
/// exactly the byte delta of the earlier edits — the source corruption in
/// #6148. Canonicalizing collapses the spellings so all edits for a file sort
/// and apply together.
///
/// Falls back to the original spelling when the path cannot be canonicalized;
/// a missing file fails later with a better message than this function could.
pub(super) fn edit_group_key(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|canonical| canonical.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
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

pub(super) fn apply_file_edits(path: &Path, edits: &[FixEditWire]) -> Result<(), String> {
    if edits.is_empty() {
        return Ok(());
    }
    let result = edited_source(path, edits)?;
    // A repair that writes source the parser rejects is never the right answer,
    // and the caller cannot tell the difference afterwards: `--apply` reported
    // "post-apply diagnostics: 0" over a file it had just made unparseable,
    // because a file that does not parse contributes no diagnostics to count.
    // Fail the pass instead, and name the edits so the bad span is visible
    // rather than something the reader has to reconstruct from the wreckage.
    harn_parser::parse_source(&result).map_err(|errors| {
        format!(
            "repair produced invalid Harn syntax for {}; no files from this pass were written: {errors:?}\napplied edits:\n{}",
            path.display(),
            describe_edits(&result, edits)
        )
    })?;
    std::fs::write(path, result)
        .map_err(|error| format!("failed to write {}: {error}", path.display()))
}

/// Render the edits of a rejected pass so the offending span is legible.
fn describe_edits(edited: &str, edits: &[FixEditWire]) -> String {
    let mut sorted = edits.to_vec();
    sorted.sort_by_key(|edit| (edit.span.start, edit.span.end));
    sorted
        .iter()
        .map(|edit| {
            let head = edited
                .get(..edit.span.start)
                .map(|prefix| {
                    let start = prefix.len().saturating_sub(30);
                    prefix.get(start..).unwrap_or(prefix)
                })
                .unwrap_or("<out of range>");
            format!(
                "  {}..{} after {head:?} -> {:?}",
                edit.span.start, edit.span.end, edit.replacement
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
pub(super) fn apply_capability_file_edits(
    path: &Path,
    edits: &[FixEditWire],
) -> Result<(), String> {
    if edits.is_empty() {
        return Ok(());
    }
    let edited = edited_source(path, edits)?;
    let candidate = format_capability_candidate(path, &edited)?;
    harn_parser::parse_source(&candidate).map_err(|error| {
        format!(
            "capability migration produced invalid syntax for {}: {error}",
            path.display()
        )
    })?;
    std::fs::write(path, candidate)
        .map_err(|error| format!("failed to write {}: {error}", path.display()))
}

fn format_capability_candidate(path: &Path, source: &str) -> Result<String, String> {
    let config = match harn_modules::project_config::load_for_path(path) {
        Ok(config) => config,
        Err(error) => {
            eprintln!(
                "warning: failed to load formatter config for {}: {error}; using defaults",
                path.display()
            );
            harn_modules::project_config::HarnConfig::default()
        }
    };
    let mut options = harn_fmt::FmtOptions::default();
    if let Some(line_width) = config.fmt.line_width {
        options.line_width = line_width;
    }
    if let Some(separator_width) = config.fmt.separator_width {
        options.separator_width = separator_width;
    }
    harn_fmt::format_source_opts(source, &options).map_err(|error| {
        format!(
            "failed to format capability migration output {}: {error}",
            path.display()
        )
    })
}

fn edited_source(path: &Path, edits: &[FixEditWire]) -> Result<String, String> {
    let mut result = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let mut sorted = edits.to_vec();
    // Two edits can share a start when one inserts an argument where another
    // rewrites the expression that begins there. Applying the wider one first
    // keeps the narrower edit's offsets valid.
    sorted.sort_by_key(|edit| {
        (
            std::cmp::Reverse(edit.span.start),
            std::cmp::Reverse(edit.span.end),
        )
    });
    validate_edit_composition(path, &sorted)?;
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
    Ok(result)
}

pub(super) fn validate_edit_composition(path: &Path, edits: &[FixEditWire]) -> Result<(), String> {
    for (index, edit) in edits.iter().enumerate() {
        for other in &edits[index + 1..] {
            let edit_inserts = edit.span.start == edit.span.end;
            let other_inserts = other.span.start == other.span.end;
            let distinct_insertions_same_offset = edit_inserts
                && other_inserts
                && edit.span.start == other.span.start
                && edit.replacement != other.replacement;
            let insertion_strictly_inside = (edit_inserts
                && other.span.start < edit.span.start
                && edit.span.start < other.span.end)
                || (other_inserts
                    && edit.span.start < other.span.start
                    && other.span.start < edit.span.end);
            let replacement_overlap = !edit_inserts
                && !other_inserts
                && edit.span.start.max(other.span.start) < edit.span.end.min(other.span.end);
            if distinct_insertions_same_offset || insertion_strictly_inside || replacement_overlap {
                return Err(format!(
                    "repair edits overlap in {} at {}..{} ({:?}) and {}..{} ({:?}); refusing to write an ambiguous candidate",
                    path.display(),
                    edit.span.start,
                    edit.span.end,
                    edit.replacement,
                    other.span.start,
                    other.span.end,
                    other.replacement,
                ));
            }
        }
    }
    Ok(())
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
