use std::path::{Path, PathBuf};

use crate::cli::{
    CrystallizeArgs, CrystallizeCommand, CrystallizeShadowArgs, CrystallizeValidateArgs,
};

pub(crate) fn run(args: CrystallizeArgs) -> Result<(), String> {
    match args.command {
        Some(CrystallizeCommand::Validate(validate)) => run_validate(validate),
        Some(CrystallizeCommand::Shadow(shadow)) => run_shadow(shadow),
        None => run_mine(args),
    }
}

fn run_mine(args: CrystallizeArgs) -> Result<(), String> {
    let from = args
        .from
        .as_deref()
        .ok_or_else(|| "--from is required when no subcommand is given".to_string())?;
    let out = args
        .out
        .as_deref()
        .ok_or_else(|| "--out is required when no subcommand is given".to_string())?;
    let report = args
        .report
        .as_deref()
        .ok_or_else(|| "--report is required when no subcommand is given".to_string())?;

    let trace_dir = PathBuf::from(from);
    let traces = harn_vm::orchestration::load_crystallization_traces_from_dir(&trace_dir)
        .map_err(|error| error.to_string())?;
    let normalized = traces.clone();
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

    let bundle = if args.bundle.is_some() {
        Some(
            harn_vm::orchestration::build_crystallization_bundle(
                artifacts.clone(),
                &normalized,
                harn_vm::orchestration::BundleOptions {
                    external_key: args.bundle_external_key.clone(),
                    title: args.bundle_title.clone(),
                    team: args.bundle_team.clone(),
                    repo: args.bundle_repo.clone(),
                    risk_level: args.bundle_risk_level.clone(),
                    rollout_policy: args.bundle_rollout_policy.clone(),
                },
            )
            .map_err(|error| error.to_string())?,
        )
    } else {
        None
    };

    let report_struct = harn_vm::orchestration::write_crystallization_artifacts(
        artifacts,
        Path::new(out),
        Path::new(report),
        args.eval_pack.as_deref().map(Path::new),
    )
    .map_err(|error| error.to_string())?;

    let selected = report_struct
        .selected_candidate_id
        .as_deref()
        .unwrap_or("none");
    println!(
        "Crystallization: selected={selected} candidates={} rejected={} traces={}",
        report_struct.candidates.len(),
        report_struct.rejected_candidates.len(),
        report_struct.source_trace_count
    );
    println!("Workflow: {out}");
    println!("Report: {report}");
    if let Some(path) = args.eval_pack.as_deref() {
        println!("Eval pack: {path}");
    }

    if let (Some(bundle_dir), Some(bundle)) = (args.bundle.as_deref(), bundle) {
        let manifest =
            harn_vm::orchestration::write_crystallization_bundle(&bundle, Path::new(bundle_dir))
                .map_err(|error| error.to_string())?;
        println!(
            "Bundle: {bundle_dir} (kind={:?} schema_version={} fixtures={})",
            manifest.kind,
            manifest.schema_version,
            manifest.fixtures.len()
        );
    }

    if report_struct.selected_candidate_id.is_none() {
        return Err("no safe crystallization candidate was proposed".to_string());
    }

    Ok(())
}

fn run_validate(args: CrystallizeValidateArgs) -> Result<(), String> {
    let validation =
        harn_vm::orchestration::validate_crystallization_bundle(Path::new(&args.bundle_dir))
            .map_err(|error| error.to_string())?;
    println!(
        "Bundle: {} (schema={} schema_version={} kind={:?} candidate_id={})",
        validation.bundle_dir,
        if validation.schema.is_empty() {
            "(unknown)".to_string()
        } else {
            validation.schema.clone()
        },
        validation.schema_version,
        validation.kind,
        if validation.candidate_id.is_empty() {
            "-"
        } else {
            validation.candidate_id.as_str()
        }
    );
    println!(
        "Checks: manifest={} workflow={} report={} eval_pack={} fixtures={} redaction={}",
        ok_label(validation.manifest_ok),
        ok_label(validation.workflow_ok),
        ok_label(validation.report_ok),
        ok_label(validation.eval_pack_ok),
        ok_label(validation.fixtures_ok),
        ok_label(validation.redaction_ok),
    );
    if validation.problems.is_empty() {
        println!("OK");
        Ok(())
    } else {
        for problem in &validation.problems {
            eprintln!("- {problem}");
        }
        Err(format!(
            "bundle validation failed with {} problem(s)",
            validation.problems.len()
        ))
    }
}

fn run_shadow(args: CrystallizeShadowArgs) -> Result<(), String> {
    let (manifest, shadow) =
        harn_vm::orchestration::shadow_replay_bundle(Path::new(&args.bundle_dir))
            .map_err(|error| error.to_string())?;
    println!(
        "Shadow replay: bundle={} candidate_id={} compared={} pass={}",
        args.bundle_dir, manifest.candidate_id, shadow.compared_traces, shadow.pass
    );
    if !shadow.failures.is_empty() {
        for failure in &shadow.failures {
            eprintln!("- {failure}");
        }
        return Err(format!(
            "shadow replay failed with {} failure(s)",
            shadow.failures.len()
        ));
    }
    Ok(())
}

fn ok_label(value: bool) -> &'static str {
    if value {
        "ok"
    } else {
        "fail"
    }
}
