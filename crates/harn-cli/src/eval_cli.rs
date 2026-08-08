//! Running evals from the CLI: run-record evaluation, eval-pack and persona
//! ladder reports, the packaged eval suite, and structural experiments that
//! fan out to subprocesses.

use crate::*;

pub(crate) fn eval_run_record(
    path: &str,
    compare: Option<&str>,
    structural_experiment: Option<&str>,
    argv: &[String],
    llm_mock_mode: &commands::run::CliLlmMockMode,
) {
    if let Some(experiment) = structural_experiment {
        let path_buf = PathBuf::from(path);
        if !path_buf.is_file() || path_buf.extension().and_then(|ext| ext.to_str()) != Some("harn")
        {
            eprintln!(
                "--structural-experiment currently requires a .harn pipeline path, got {path}"
            );
            process::exit(1);
        }
        if compare.is_some() {
            eprintln!("--compare cannot be combined with --structural-experiment");
            process::exit(1);
        }
        if matches!(llm_mock_mode, commands::run::CliLlmMockMode::Record { .. }) {
            eprintln!("--llm-mock-record cannot be combined with --structural-experiment");
            process::exit(1);
        }
        let path_buf = fs::canonicalize(&path_buf).unwrap_or_else(|error| {
            command_error(&format!(
                "failed to canonicalize structural eval pipeline {}: {error}",
                path_buf.display()
            ))
        });
        run_structural_experiment_eval(&path_buf, experiment, argv, llm_mock_mode);
        return;
    }

    let path_buf = PathBuf::from(path);
    if path_buf.is_file() && file_looks_like_persona_eval_ladder_manifest(&path_buf) {
        if compare.is_some() {
            eprintln!("--compare is not supported with persona eval ladder manifests");
            process::exit(1);
        }
        let manifest = load_persona_eval_ladder_manifest_or_exit(&path_buf);
        let report =
            harn_vm::orchestration::run_persona_eval_ladder(&manifest).unwrap_or_else(|error| {
                eprintln!(
                    "Failed to evaluate persona eval ladder {}: {error}",
                    path_buf.display()
                );
                process::exit(1);
            });
        print_persona_ladder_report(&report);
        if !report.pass {
            process::exit(1);
        }
        return;
    }

    if path_buf.is_file() && file_looks_like_eval_pack_manifest(&path_buf) {
        if compare.is_some() {
            eprintln!("--compare is not supported with eval pack manifests");
            process::exit(1);
        }
        let manifest = load_eval_pack_manifest_or_exit(&path_buf);
        let report = harn_vm::orchestration::evaluate_eval_pack_manifest_resumable(&manifest, None)
            .unwrap_or_else(|error| {
                eprintln!(
                    "Failed to evaluate eval pack {}: {error}",
                    path_buf.display()
                );
                process::exit(1);
            });
        print_eval_pack_report(&report);
        if !report.pass {
            process::exit(1);
        }
        return;
    }

    if path_buf.is_file() && file_looks_like_eval_manifest(&path_buf) {
        if compare.is_some() {
            eprintln!("--compare is not supported with eval suite manifests");
            process::exit(1);
        }
        let manifest = load_eval_suite_manifest_or_exit(&path_buf);
        let suite = harn_vm::orchestration::evaluate_run_suite_manifest(&manifest).unwrap_or_else(
            |error| {
                eprintln!(
                    "Failed to evaluate manifest {}: {error}",
                    path_buf.display()
                );
                process::exit(1);
            },
        );
        println!(
            "{} {} passed, {} failed, {} total",
            if suite.pass { "PASS" } else { "FAIL" },
            suite.passed,
            suite.failed,
            suite.total
        );
        for case in &suite.cases {
            println!(
                "- {} [{}] {}",
                case.label.clone().unwrap_or_else(|| case.run_id.clone()),
                case.workflow_id,
                if case.pass { "PASS" } else { "FAIL" }
            );
            if let Some(path) = &case.source_path {
                println!("  path: {path}");
            }
            if let Some(comparison) = &case.comparison {
                println!("  baseline identical: {}", comparison.identical);
                if !comparison.identical {
                    println!(
                        "  baseline status: {} -> {}",
                        comparison.left_status, comparison.right_status
                    );
                }
            }
            for failure in &case.failures {
                println!("  {failure}");
            }
        }
        if !suite.pass {
            process::exit(1);
        }
        return;
    }

    let paths = collect_run_record_paths(path);
    if paths.len() > 1 {
        let mut cases = Vec::new();
        for path in &paths {
            let run = load_run_record_or_exit(path);
            let fixture = run
                .replay_fixture
                .clone()
                .unwrap_or_else(|| harn_vm::orchestration::replay_fixture_from_run(&run));
            cases.push((run, fixture, Some(path.display().to_string())));
        }
        let suite = harn_vm::orchestration::evaluate_run_suite(cases);
        println!(
            "{} {} passed, {} failed, {} total",
            if suite.pass { "PASS" } else { "FAIL" },
            suite.passed,
            suite.failed,
            suite.total
        );
        for case in &suite.cases {
            println!(
                "- {} [{}] {}",
                case.run_id,
                case.workflow_id,
                if case.pass { "PASS" } else { "FAIL" }
            );
            if let Some(path) = &case.source_path {
                println!("  path: {path}");
            }
            if let Some(comparison) = &case.comparison {
                println!("  baseline identical: {}", comparison.identical);
            }
            for failure in &case.failures {
                println!("  {failure}");
            }
        }
        if !suite.pass {
            process::exit(1);
        }
        return;
    }

    let run = load_run_record_or_exit(&paths[0]);
    let fixture = run
        .replay_fixture
        .clone()
        .unwrap_or_else(|| harn_vm::orchestration::replay_fixture_from_run(&run));
    let report = harn_vm::orchestration::evaluate_run_against_fixture(&run, &fixture);
    println!("{}", if report.pass { "PASS" } else { "FAIL" });
    println!("Stages: {}", report.stage_count);
    if let Some(compare_path) = compare {
        let baseline = load_run_record_or_exit(Path::new(compare_path));
        print_run_diff(&harn_vm::orchestration::diff_run_records(&baseline, &run));
    }
    if !report.failures.is_empty() {
        for failure in &report.failures {
            println!("- {failure}");
        }
    }
    if !report.pass {
        process::exit(1);
    }
}

pub(crate) fn print_eval_pack_report(report: &harn_vm::orchestration::EvalPackReport) {
    println!(
        "{} {} passed, {} blocking failed, {} warning, {} informational, {} total",
        if report.pass { "PASS" } else { "FAIL" },
        report.passed,
        report.blocking_failed,
        report.warning_failed,
        report.informational_failed,
        report.total
    );
    for case in &report.cases {
        println!(
            "- {} [{}] {} ({})",
            case.label,
            case.workflow_id,
            if case.pass { "PASS" } else { "FAIL" },
            case.severity
        );
        if let Some(path) = &case.source_path {
            println!("  path: {path}");
        }
        if let Some(comparison) = &case.comparison {
            println!("  baseline identical: {}", comparison.identical);
            if !comparison.identical {
                println!(
                    "  baseline status: {} -> {}",
                    comparison.left_status, comparison.right_status
                );
            }
        }
        for failure in &case.failures {
            println!("  {failure}");
        }
        for warning in &case.warnings {
            println!("  warning: {warning}");
        }
        for item in &case.informational {
            println!("  info: {item}");
        }
    }
    for ladder in &report.ladders {
        println!(
            "- ladder {} [{}] {} ({}) first_correct={}/{}",
            ladder.id,
            ladder.persona,
            if ladder.pass { "PASS" } else { "FAIL" },
            ladder.severity,
            ladder.first_correct_route.as_deref().unwrap_or("<none>"),
            ladder.first_correct_tier.as_deref().unwrap_or("<none>")
        );
        println!("  artifacts: {}", ladder.artifact_root);
        for tier in &ladder.tiers {
            println!(
                "  - {} [{}] {} tools={} models={} latency={}ms cost=${:.6}",
                tier.timeout_tier,
                tier.route_id,
                tier.outcome,
                tier.tool_calls,
                tier.model_calls,
                tier.latency_ms,
                tier.cost_usd
            );
            for reason in &tier.degradation_reasons {
                println!("    {reason}");
            }
        }
    }
}

pub(crate) fn print_persona_ladder_report(
    report: &harn_vm::orchestration::PersonaEvalLadderReport,
) {
    println!(
        "{} ladder {} passed, {} degraded/looped, {} total",
        if report.pass { "PASS" } else { "FAIL" },
        report.passed,
        report.failed,
        report.total
    );
    println!(
        "first_correct: {}/{}",
        report.first_correct_route.as_deref().unwrap_or("<none>"),
        report.first_correct_tier.as_deref().unwrap_or("<none>")
    );
    println!("artifacts: {}", report.artifact_root);
    for tier in &report.tiers {
        println!(
            "- {} [{}] {} tools={} models={} latency={}ms cost=${:.6}",
            tier.timeout_tier,
            tier.route_id,
            tier.outcome,
            tier.tool_calls,
            tier.model_calls,
            tier.latency_ms,
            tier.cost_usd
        );
        for reason in &tier.degradation_reasons {
            println!("  {reason}");
        }
    }
}

pub(crate) fn run_package_evals() {
    let paths = package::load_package_eval_pack_paths(None).unwrap_or_else(|error| {
        eprintln!("{error}");
        process::exit(1);
    });
    let mut all_pass = true;
    for path in &paths {
        println!("Eval pack: {}", path.display());
        let manifest = load_eval_pack_manifest_or_exit(path);
        let report = harn_vm::orchestration::evaluate_eval_pack_manifest_resumable(&manifest, None)
            .unwrap_or_else(|error| {
                eprintln!("Failed to evaluate eval pack {}: {error}", path.display());
                process::exit(1);
            });
        print_eval_pack_report(&report);
        all_pass &= report.pass;
    }
    if !all_pass {
        process::exit(1);
    }
}

pub(crate) fn run_structural_experiment_eval(
    path: &Path,
    experiment: &str,
    argv: &[String],
    llm_mock_mode: &commands::run::CliLlmMockMode,
) {
    let baseline_dir = tempfile::Builder::new()
        .prefix("harn-eval-baseline-")
        .tempdir()
        .unwrap_or_else(|error| {
            command_error(&format!("failed to create baseline tempdir: {error}"))
        });
    let variant_dir = tempfile::Builder::new()
        .prefix("harn-eval-variant-")
        .tempdir()
        .unwrap_or_else(|error| {
            command_error(&format!("failed to create variant tempdir: {error}"))
        });

    let baseline = spawn_eval_pipeline_run(path, baseline_dir.path(), None, argv, llm_mock_mode);
    if !baseline.status.success() {
        relay_subprocess_failure("baseline", &baseline);
    }

    let variant = spawn_eval_pipeline_run(
        path,
        variant_dir.path(),
        Some(experiment),
        argv,
        llm_mock_mode,
    );
    if !variant.status.success() {
        relay_subprocess_failure("variant", &variant);
    }

    let baseline_runs = collect_structural_eval_runs(baseline_dir.path());
    let variant_runs = collect_structural_eval_runs(variant_dir.path());
    if baseline_runs.is_empty() || variant_runs.is_empty() {
        eprintln!(
            "structural eval expected workflow run records under {} and {}, but one side was empty",
            baseline_dir.path().display(),
            variant_dir.path().display()
        );
        process::exit(1);
    }
    if baseline_runs.len() != variant_runs.len() {
        eprintln!(
            "structural eval produced different run counts: baseline={} variant={}",
            baseline_runs.len(),
            variant_runs.len()
        );
        process::exit(1);
    }

    let mut baseline_ok = 0usize;
    let mut variant_ok = 0usize;
    let mut any_failures = false;

    println!("Structural experiment: {experiment}");
    println!("Cases: {}", baseline_runs.len());
    for (baseline_run, variant_run) in baseline_runs.iter().zip(variant_runs.iter()) {
        let baseline_fixture = baseline_run
            .replay_fixture
            .clone()
            .unwrap_or_else(|| harn_vm::orchestration::replay_fixture_from_run(baseline_run));
        let variant_fixture = variant_run
            .replay_fixture
            .clone()
            .unwrap_or_else(|| harn_vm::orchestration::replay_fixture_from_run(variant_run));
        let baseline_report =
            harn_vm::orchestration::evaluate_run_against_fixture(baseline_run, &baseline_fixture);
        let variant_report =
            harn_vm::orchestration::evaluate_run_against_fixture(variant_run, &variant_fixture);
        let diff = harn_vm::orchestration::diff_run_records(baseline_run, variant_run);
        if baseline_report.pass {
            baseline_ok += 1;
        }
        if variant_report.pass {
            variant_ok += 1;
        }
        any_failures |= !baseline_report.pass || !variant_report.pass;
        println!(
            "- {} [{}]",
            variant_run
                .workflow_name
                .clone()
                .unwrap_or_else(|| variant_run.workflow_id.clone()),
            variant_run.task
        );
        println!(
            "  baseline: {}",
            if baseline_report.pass { "PASS" } else { "FAIL" }
        );
        for failure in &baseline_report.failures {
            println!("    {failure}");
        }
        println!(
            "  variant: {}",
            if variant_report.pass { "PASS" } else { "FAIL" }
        );
        for failure in &variant_report.failures {
            println!("    {failure}");
        }
        println!("  diff identical: {}", diff.identical);
        println!("  stage diffs: {}", diff.stage_diffs.len());
        println!("  tool diffs: {}", diff.tool_diffs.len());
        println!("  observability diffs: {}", diff.observability_diffs.len());
    }

    println!("Baseline {} / {} passed", baseline_ok, baseline_runs.len());
    println!("Variant {} / {} passed", variant_ok, variant_runs.len());

    if any_failures {
        process::exit(1);
    }
}

pub(crate) fn spawn_eval_pipeline_run(
    path: &Path,
    run_dir: &Path,
    structural_experiment: Option<&str>,
    argv: &[String],
    llm_mock_mode: &commands::run::CliLlmMockMode,
) -> std::process::Output {
    let exe = env::current_exe().unwrap_or_else(|error| {
        command_error(&format!("failed to resolve current executable: {error}"))
    });
    let mut command = std::process::Command::new(exe);
    command.current_dir(path.parent().unwrap_or_else(|| Path::new(".")));
    command.arg("run");
    match llm_mock_mode {
        commands::run::CliLlmMockMode::Off => {}
        commands::run::CliLlmMockMode::Replay { fixture_path } => {
            command
                .arg("--llm-mock")
                .arg(absolute_cli_path(fixture_path));
        }
        commands::run::CliLlmMockMode::Record { fixture_path } => {
            command
                .arg("--llm-mock-record")
                .arg(absolute_cli_path(fixture_path));
        }
    }
    command.arg(path);
    if !argv.is_empty() {
        command.arg("--");
        command.args(argv);
    }
    command.env(harn_vm::runtime_paths::HARN_RUN_DIR_ENV, run_dir);
    if let Some(experiment) = structural_experiment {
        command.env("HARN_STRUCTURAL_EXPERIMENT", experiment);
    }
    command.output().unwrap_or_else(|error| {
        command_error(&format!(
            "failed to spawn `harn run {}` for structural eval: {error}",
            path.display()
        ))
    })
}

pub(crate) fn absolute_cli_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(path)
}

pub(crate) fn relay_subprocess_failure(label: &str, output: &std::process::Output) -> ! {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stdout.trim().is_empty() {
        eprintln!("[{label}] stdout:\n{stdout}");
    }
    if !stderr.trim().is_empty() {
        eprintln!("[{label}] stderr:\n{stderr}");
    }
    process::exit(output.status.code().unwrap_or(1));
}

pub(crate) fn collect_structural_eval_runs(dir: &Path) -> Vec<harn_vm::orchestration::RunRecord> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|error| {
            command_error(&format!(
                "failed to read structural eval run dir {}: {error}",
                dir.display()
            ))
        })
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|entry| entry.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect();
    paths.sort();
    let mut runs: Vec<_> = paths
        .iter()
        .map(|path| load_run_record_or_exit(path))
        .collect();
    runs.sort_by(|left, right| {
        (
            left.started_at.as_str(),
            left.workflow_id.as_str(),
            left.task.as_str(),
        )
            .cmp(&(
                right.started_at.as_str(),
                right.workflow_id.as_str(),
                right.task.as_str(),
            ))
    });
    runs
}
