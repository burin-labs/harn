use super::*;

pub(super) fn print_human_plan(plan: &RepairPlan) {
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

pub(super) fn print_apply_result(result: &ApplyResult) {
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

pub(super) fn skipped_files_error(count: usize) -> String {
    format!("harn fix skipped {count} file(s) due to read, lex, or parse errors")
}
