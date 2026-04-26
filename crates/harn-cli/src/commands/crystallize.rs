use std::path::Path;

use crate::cli::CrystallizeArgs;

pub(crate) fn run(args: CrystallizeArgs) -> Result<(), String> {
    let traces =
        harn_vm::orchestration::load_crystallization_traces_from_dir(Path::new(&args.from))
            .map_err(|error| error.to_string())?;
    let artifacts = harn_vm::orchestration::crystallize_traces(
        traces,
        harn_vm::orchestration::CrystallizeOptions {
            min_examples: args.min_examples,
            workflow_name: args.workflow_name.clone(),
            package_name: args.package_name.clone(),
            author: args.author.clone(),
            approver: args.approver.clone(),
            eval_pack_link: args.eval_pack.clone(),
        },
    )
    .map_err(|error| error.to_string())?;

    let report = harn_vm::orchestration::write_crystallization_artifacts(
        artifacts,
        Path::new(&args.out),
        Path::new(&args.report),
        args.eval_pack.as_deref().map(Path::new),
    )
    .map_err(|error| error.to_string())?;

    let selected = report.selected_candidate_id.as_deref().unwrap_or("none");
    println!(
        "Crystallization: selected={selected} candidates={} rejected={} traces={}",
        report.candidates.len(),
        report.rejected_candidates.len(),
        report.source_trace_count
    );
    println!("Workflow: {}", args.out);
    println!("Report: {}", args.report);
    if let Some(path) = args.eval_pack.as_deref() {
        println!("Eval pack: {path}");
    }

    if report.selected_candidate_id.is_none() {
        return Err("no safe crystallization candidate was proposed".to_string());
    }

    Ok(())
}
