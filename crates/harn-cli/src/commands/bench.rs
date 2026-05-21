use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::time::Instant;

use harn_parser::DiagnosticSeverity;
use serde::{Deserialize, Serialize};

use crate::cli::{BenchArgs, BenchCommand, BenchReplayArgs};
use crate::commands::run::{connect_mcp_servers, RunProfileOptions};
use crate::package;
use crate::parse_source_file;

#[derive(Debug, Clone, Serialize)]
struct BenchRun {
    iteration: usize,
    wall_time_ms: f64,
    llm_time_ms: i64,
    input_tokens: i64,
    output_tokens: i64,
    call_count: i64,
    total_cost_usd: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    profile: Option<harn_vm::profile::RunProfile>,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct BenchStats {
    iterations: usize,
    min_ms: f64,
    mean_ms: f64,
    p50_ms: f64,
    p95_ms: f64,
    max_ms: f64,
    stddev_ms: f64,
    total_ms: f64,
}

pub(crate) async fn run(args: BenchArgs) {
    match args.command {
        Some(BenchCommand::Replay(replay)) => {
            if let Err(error) = run_replay_bench(replay) {
                eprintln!("error: {error}");
                process::exit(1);
            }
        }
        None => {
            let Some(path) = args.file.as_deref() else {
                eprintln!("error: `harn bench` requires a .harn file or a subcommand");
                process::exit(1);
            };
            run_bench(
                path,
                args.iterations,
                crate::run_profile_options(&args.profile),
            )
            .await;
        }
    }
}

pub(crate) async fn run_bench(path: &str, iterations: usize, profile: RunProfileOptions) {
    if iterations == 0 {
        eprintln!("error: `harn bench` requires at least one iteration");
        process::exit(1);
    }

    let (source, program) = parse_source_file(path);
    let file_path = Path::new(path);
    if let Err(error) = package::ensure_dependencies_materialized(file_path) {
        eprintln!("error: {error}");
        process::exit(1);
    }
    let graph = harn_modules::build(&[file_path.to_path_buf()]);
    let mut checker = harn_parser::TypeChecker::new();
    if let Some(imported) = graph.imported_names_for_file(file_path) {
        checker = checker.with_imported_names(imported);
    }
    if let Some(imported) = graph.imported_type_declarations_for_file(file_path) {
        checker = checker.with_imported_type_decls(imported);
    }
    if let Some(imported) = graph.imported_callable_declarations_for_file(file_path) {
        checker = checker.with_imported_callable_decls(imported);
    }
    let type_diagnostics = checker.check_with_source(&program, &source);
    let mut had_type_error = false;
    for diag in &type_diagnostics {
        match diag.severity {
            DiagnosticSeverity::Error => {
                had_type_error = true;
                let rendered = harn_parser::diagnostic::render_type_diagnostic(&source, path, diag);
                eprint!("{rendered}");
            }
            DiagnosticSeverity::Warning => {
                let rendered = harn_parser::diagnostic::render_type_diagnostic(&source, path, diag);
                eprint!("{rendered}");
            }
        }
    }
    if had_type_error {
        process::exit(1);
    }

    let chunk = match harn_vm::Compiler::new().compile(&program) {
        Ok(chunk) => chunk,
        Err(error) => {
            eprintln!("error: compile error: {error}");
            process::exit(1);
        }
    };

    let source_parent = Path::new(path).parent().unwrap_or(Path::new("."));
    let project_root = harn_vm::stdlib::process::find_project_root(source_parent);
    let store_base = project_root.as_deref().unwrap_or(source_parent);
    let pipeline_name = Path::new(path)
        .file_stem()
        .and_then(|segment| segment.to_str())
        .unwrap_or("default");
    let extensions = package::load_runtime_extensions(Path::new(path));
    package::install_runtime_extensions(&extensions);

    let mut runs = Vec::with_capacity(iterations);
    let mut profile_span_groups = Vec::new();
    for iteration in 0..iterations {
        harn_vm::reset_thread_local_state();
        harn_vm::llm::enable_tracing();
        if profile.is_enabled() {
            harn_vm::tracing::set_tracing_enabled(true);
        }

        let mut vm = harn_vm::Vm::new();
        harn_vm::register_vm_stdlib_with_deferred_llm(&mut vm);
        crate::install_default_hostlib(&mut vm);
        harn_vm::register_store_builtins(&mut vm, store_base);
        harn_vm::register_metadata_builtins(&mut vm, store_base);
        harn_vm::register_checkpoint_builtins(&mut vm, store_base, pipeline_name);
        vm.set_source_info(path, &source);
        if let Some(ref root) = project_root {
            vm.set_project_root(root);
        }
        if !source_parent.as_os_str().is_empty() {
            vm.set_source_dir(source_parent);
        }
        vm.set_harness(harn_vm::Harness::real());

        if let Some(manifest) = extensions.root_manifest.as_ref() {
            if !manifest.mcp.is_empty() {
                connect_mcp_servers(&manifest.mcp, &mut vm).await;
            }
        }
        if let Err(error) = package::install_manifest_triggers(&mut vm, &extensions).await {
            eprintln!("error: failed to install manifest triggers: {error}");
            process::exit(1);
        }
        if let Err(error) = package::install_manifest_hooks(&mut vm, &extensions).await {
            eprintln!("error: failed to install manifest hooks: {error}");
            process::exit(1);
        }

        let started_at = Instant::now();
        let local = tokio::task::LocalSet::new();
        let execution_result = local.run_until(async { vm.execute(&chunk).await }).await;
        let wall_time_ms = started_at.elapsed().as_secs_f64() * 1000.0;

        match execution_result {
            Ok(_) => {
                let (input_tokens, output_tokens, llm_time_ms, call_count) =
                    harn_vm::llm::peek_trace_summary();
                let run_profile = if profile.is_enabled() {
                    let spans = harn_vm::tracing::take_spans();
                    let rollup = harn_vm::profile::build(&spans);
                    profile_span_groups.push(spans);
                    Some(rollup)
                } else {
                    None
                };
                runs.push(BenchRun {
                    iteration: iteration + 1,
                    wall_time_ms,
                    llm_time_ms,
                    input_tokens,
                    output_tokens,
                    call_count,
                    total_cost_usd: harn_vm::llm::peek_total_cost(),
                    profile: run_profile,
                });
            }
            Err(error) => {
                eprint!("{}", vm.format_runtime_error(&error));
                eprintln!("benchmark aborted on iteration {}", iteration + 1);
                process::exit(1);
            }
        }
    }

    let aggregate_profile = if profile.is_enabled() {
        Some(harn_vm::profile::build_aggregate(&profile_span_groups))
    } else {
        None
    };
    print!(
        "{}",
        render_bench_report(path, &runs, profile.text, aggregate_profile.as_ref())
    );
    if let Some(json_path) = profile.json_path.as_ref() {
        if let Err(error) =
            write_bench_profile_json(json_path, path, &runs, aggregate_profile.as_ref())
        {
            eprintln!("warning: failed to write benchmark profile: {error}");
        }
    }
}

fn render_bench_report(
    path: &str,
    runs: &[BenchRun],
    include_profile: bool,
    aggregate_profile: Option<&harn_vm::profile::RunProfile>,
) -> String {
    let stats = bench_stats(runs);
    let total_llm = runs.iter().map(|run| run.llm_time_ms).sum::<i64>();
    let total_input = runs.iter().map(|run| run.input_tokens).sum::<i64>();
    let total_output = runs.iter().map(|run| run.output_tokens).sum::<i64>();
    let total_calls = runs.iter().map(|run| run.call_count).sum::<i64>();
    let total_cost = runs.iter().map(|run| run.total_cost_usd).sum::<f64>();
    let iterations = stats.iterations as f64;

    let mut report = format!(
        "\
Benchmark: {path}
Iterations: {}
Wall time: min {:.2} ms | mean {:.2} ms | p50 {:.2} ms | p95 {:.2} ms | max {:.2} ms | stddev {:.2} ms | total {:.2} ms
LLM time: total {} ms | avg {:.2} ms/run
LLM calls: total {} | avg {:.2}/run
Input tokens: total {} | avg {:.2}/run
Output tokens: total {} | avg {:.2}/run
Cost: total ${:.4} | avg ${:.4}/run
",
        stats.iterations,
        stats.min_ms,
        stats.mean_ms,
        stats.p50_ms,
        stats.p95_ms,
        stats.max_ms,
        stats.stddev_ms,
        stats.total_ms,
        total_llm,
        total_llm as f64 / iterations,
        total_calls,
        total_calls as f64 / iterations,
        total_input,
        total_input as f64 / iterations,
        total_output,
        total_output as f64 / iterations,
        total_cost,
        total_cost / iterations,
    );
    if include_profile {
        if let Some(profile) = aggregate_profile {
            report.push_str(&harn_vm::profile::render(profile));
        }
    }
    report
}

fn bench_stats(runs: &[BenchRun]) -> BenchStats {
    let mut sorted = runs.iter().map(|run| run.wall_time_ms).collect::<Vec<_>>();
    sorted.sort_by(f64::total_cmp);
    let total_ms = sorted.iter().sum::<f64>();
    let iterations = sorted.len();
    let mean_ms = total_ms / iterations as f64;
    let variance = sorted
        .iter()
        .map(|ms| {
            let delta = ms - mean_ms;
            delta * delta
        })
        .sum::<f64>()
        / iterations as f64;
    BenchStats {
        iterations,
        min_ms: sorted[0],
        mean_ms,
        p50_ms: percentile_sorted(&sorted, 0.50),
        p95_ms: percentile_sorted(&sorted, 0.95),
        max_ms: sorted[iterations - 1],
        stddev_ms: variance.sqrt(),
        total_ms,
    }
}

fn percentile_sorted(sorted: &[f64], percentile: f64) -> f64 {
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = percentile.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    if lower == upper {
        sorted[lower]
    } else {
        let weight = rank - lower as f64;
        sorted[lower] * (1.0 - weight) + sorted[upper] * weight
    }
}

#[derive(Serialize)]
struct BenchJsonReport<'a> {
    path: &'a str,
    iterations: &'a [BenchRun],
    min_ms: f64,
    mean_ms: f64,
    p50_ms: f64,
    p95_ms: f64,
    max_ms: f64,
    stddev_ms: f64,
    total_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    rollup: Option<&'a harn_vm::profile::RunProfile>,
}

fn write_bench_profile_json(
    json_path: &Path,
    bench_path: &str,
    runs: &[BenchRun],
    aggregate_profile: Option<&harn_vm::profile::RunProfile>,
) -> Result<(), String> {
    if let Some(parent) = json_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("create {}: {error}", parent.display()))?;
        }
    }
    let stats = bench_stats(runs);
    let report = BenchJsonReport {
        path: bench_path,
        iterations: runs,
        min_ms: stats.min_ms,
        mean_ms: stats.mean_ms,
        p50_ms: stats.p50_ms,
        p95_ms: stats.p95_ms,
        max_ms: stats.max_ms,
        stddev_ms: stats.stddev_ms,
        total_ms: stats.total_ms,
        rollup: aggregate_profile,
    };
    let json = serde_json::to_string_pretty(&report)
        .map_err(|error| format!("serialize benchmark profile: {error}"))?;
    fs::write(json_path, json).map_err(|error| format!("write {}: {error}", json_path.display()))
}

const REPLAY_BENCHMARK_SUITE_SCHEMA_VERSION: &str = "harn.replay_benchmark.suite.v1";

#[derive(Debug, Deserialize)]
struct ReplayBenchmarkSuiteManifest {
    #[serde(default)]
    schema_version: String,
    #[serde(default)]
    name: Option<String>,
    fixtures: Vec<ReplayBenchmarkFixtureRef>,
}

#[derive(Debug, Deserialize)]
struct ReplayBenchmarkFixtureRef {
    path: String,
}

fn run_replay_bench(args: BenchReplayArgs) -> Result<(), String> {
    let repo_root = discover_repo_root().unwrap_or_else(|| PathBuf::from("."));
    let selection = resolve_replay_selection(args.selection.as_ref(), &repo_root);
    let (suite_name, fixture_paths) =
        resolve_replay_benchmark_selection(&selection, &repo_root, &args.suite_name)?;
    let mut reports = Vec::new();

    for fixture_path in fixture_paths {
        let display_path = display_path(&fixture_path);
        let trace = read_replay_trace_fixture(&fixture_path)?;
        if !matches_replay_benchmark_filter(args.filter.as_deref(), &display_path, &trace.name) {
            continue;
        }
        validate_protocol_fixture_refs(&repo_root, &trace.protocol_fixture_refs)?;
        reports.push(
            harn_vm::benchmark_replay_trace(display_path, &trace)
                .map_err(|error| error.to_string())?,
        );
    }

    if let Some(adapter_id) = args.adapter.as_deref() {
        let adapter = replay_trace_adapter(adapter_id)?;
        let first_path = args
            .external_first
            .as_ref()
            .ok_or_else(|| "--external-first is required with --adapter".to_string())?;
        let second_path = args
            .external_second
            .as_ref()
            .ok_or_else(|| "--external-second is required with --adapter".to_string())?;
        let first = fs::read_to_string(first_path)
            .map_err(|error| format!("read {}: {error}", first_path.display()))?;
        let second = fs::read_to_string(second_path)
            .map_err(|error| format!("read {}: {error}", second_path.display()))?;
        if matches_replay_benchmark_filter(
            args.filter.as_deref(),
            &format!("adapter:{adapter_id}:{}", args.external_name),
            &args.external_name,
        ) {
            reports.push(
                harn_vm::benchmark_adapted_replay_pair(
                    adapter.as_ref(),
                    args.external_name.clone(),
                    &first,
                    &second,
                )
                .map_err(|error| error.to_string())?,
            );
        }
    }

    if reports.is_empty() {
        return Err(format!(
            "no replay benchmark fixtures matched {}",
            selection.display()
        ));
    }

    let source_paths = reports
        .iter()
        .map(|report| report.path.clone())
        .collect::<Vec<_>>();
    let report = harn_vm::build_replay_benchmark_report(suite_name, source_paths, reports);
    let json = serde_json::to_string_pretty(&report)
        .map_err(|error| format!("serialize replay benchmark report: {error}"))?;

    if let Some(output_path) = args.output.as_ref() {
        write_replay_benchmark_report(output_path, &json)?;
    }
    if args.json {
        println!("{json}");
    } else {
        print!("{}", render_replay_benchmark_report(&report));
        if let Some(output_path) = args.output.as_ref() {
            println!("Report JSON: {}", output_path.display());
        }
    }

    if report.summary.failed > 0 {
        return Err(format!(
            "replay benchmark failed: {} passed, {} failed",
            report.summary.passed, report.summary.failed
        ));
    }
    Ok(())
}

fn resolve_replay_selection(selection: Option<&PathBuf>, repo_root: &Path) -> PathBuf {
    match selection {
        Some(selection) if selection.is_absolute() || selection.exists() => selection.clone(),
        Some(selection) => repo_root.join(selection),
        None => repo_root.join("benchmarks/replay/suite.json"),
    }
}

fn resolve_replay_benchmark_selection(
    selection: &Path,
    repo_root: &Path,
    fallback_suite_name: &str,
) -> Result<(String, Vec<PathBuf>), String> {
    if !selection.exists() {
        return Err(format!(
            "replay benchmark target not found: {}",
            selection.display()
        ));
    }
    if selection.is_file() {
        let text = fs::read_to_string(selection)
            .map_err(|error| format!("read {}: {error}", selection.display()))?;
        let value: serde_json::Value = serde_json::from_str(&text)
            .map_err(|error| format!("invalid JSON in {}: {error}", selection.display()))?;
        if value.get("fixtures").is_some() {
            let manifest: ReplayBenchmarkSuiteManifest =
                serde_json::from_value(value).map_err(|error| {
                    format!(
                        "invalid replay benchmark suite {}: {error}",
                        selection.display()
                    )
                })?;
            if manifest.schema_version != REPLAY_BENCHMARK_SUITE_SCHEMA_VERSION {
                return Err(format!(
                    "unsupported replay benchmark suite schema_version {:?}; expected {REPLAY_BENCHMARK_SUITE_SCHEMA_VERSION}",
                    manifest.schema_version
                ));
            }
            let base = selection.parent().unwrap_or_else(|| Path::new("."));
            let mut fixtures = Vec::with_capacity(manifest.fixtures.len());
            for fixture in manifest.fixtures {
                fixtures.push(resolve_suite_fixture_path(repo_root, base, &fixture.path));
            }
            return Ok((
                manifest
                    .name
                    .unwrap_or_else(|| fallback_suite_name.to_string()),
                fixtures,
            ));
        }
        return Ok((
            fallback_suite_name.to_string(),
            vec![selection.to_path_buf()],
        ));
    }

    let mut fixtures = Vec::new();
    collect_json_files(selection, &mut fixtures);
    if fixtures.is_empty() {
        return Err(format!(
            "no replay benchmark JSON fixtures found under {}",
            selection.display()
        ));
    }
    Ok((fallback_suite_name.to_string(), fixtures))
}

fn resolve_suite_fixture_path(repo_root: &Path, base: &Path, raw: &str) -> PathBuf {
    let path = Path::new(raw);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    let repo_relative = repo_root.join(path);
    if repo_relative.exists() {
        repo_relative
    } else {
        base.join(path)
    }
}

fn collect_json_files(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_file() {
        if path.extension().is_some_and(|ext| ext == "json") {
            out.push(path.to_path_buf());
        }
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        collect_json_files(&entry.path(), out);
    }
}

fn read_replay_trace_fixture(path: &Path) -> Result<harn_vm::ReplayOracleTrace, String> {
    let text =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|error| format!("invalid replay trace JSON in {}: {error}", path.display()))
}

fn validate_protocol_fixture_refs(repo_root: &Path, refs: &[String]) -> Result<(), String> {
    for fixture_ref in refs {
        if !fixture_ref.starts_with("conformance/protocols/fixtures/") {
            return Err(format!(
                "protocol fixture ref must point under conformance/protocols/fixtures: {fixture_ref}"
            ));
        }
        let path = Path::new(fixture_ref);
        if path.is_absolute() {
            return Err(format!(
                "protocol fixture ref must be repo-relative: {fixture_ref}"
            ));
        }
        let candidate = repo_root.join(path);
        if !candidate.is_file() {
            return Err(format!(
                "protocol fixture ref not found: {}",
                candidate.display()
            ));
        }
    }
    Ok(())
}

fn matches_replay_benchmark_filter(filter: Option<&str>, path: &str, name: &str) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    path.contains(filter) || name.contains(filter)
}

fn replay_trace_adapter(adapter_id: &str) -> Result<Box<dyn harn_vm::ReplayTraceAdapter>, String> {
    match adapter_id {
        harn_vm::OPENCODE_JSONL_ADAPTER_ID | "opencode" => {
            Ok(Box::new(harn_vm::OpenCodeJsonlAdapter))
        }
        other => Err(format!(
            "unsupported replay trace adapter `{other}`; expected `{}`",
            harn_vm::OPENCODE_JSONL_ADAPTER_ID
        )),
    }
}

fn write_replay_benchmark_report(path: &Path, json: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("create {}: {error}", parent.display()))?;
        }
    }
    fs::write(path, json).map_err(|error| format!("write {}: {error}", path.display()))
}

fn render_replay_benchmark_report(report: &harn_vm::ReplayBenchmarkReport) -> String {
    let mut out = format!(
        "\
Replay benchmark: {}
Fixtures: {} passed, {} failed
Mean replay fidelity: {:.3}
Permission preservation: {:.3}
Tool-call drift count: {}
Transcript drift count: {}
Observed interactions: {}
",
        report.suite.name,
        report.summary.passed,
        report.summary.failed,
        report.summary.mean_replay_fidelity_score,
        report.summary.mean_permission_decision_preservation_score,
        report.summary.tool_call_drift_count,
        report.summary.transcript_drift_count,
        report.summary.observed_interactions,
    );
    for fixture in &report.fixtures {
        let status = if fixture.passed { "PASS" } else { "FAIL" };
        out.push_str(&format!(
            "  {status} {} fidelity={:.3} determinism={:.3} tool_drift={} transcript_drift={}\n",
            fixture.name,
            fixture.metrics.replay_fidelity_score,
            fixture.metrics.determinism_score,
            fixture.metrics.tool_call_drift_count,
            fixture.metrics.transcript_drift_count,
        ));
        if let Some(divergence) = &fixture.first_divergence {
            out.push_str(&format!("    first divergence: {}\n", divergence.path));
        }
    }
    out
}

fn discover_repo_root() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    for ancestor in cwd.ancestors() {
        if ancestor.join("Cargo.toml").is_file() && ancestor.join("conformance").is_dir() {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

fn display_path(path: &Path) -> String {
    discover_repo_root()
        .and_then(|root| path.strip_prefix(root).ok().map(Path::to_path_buf))
        .unwrap_or_else(|| path.to_path_buf())
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::{
        bench_stats, percentile_sorted, read_replay_trace_fixture, render_bench_report,
        render_replay_benchmark_report, resolve_replay_benchmark_selection,
        validate_protocol_fixture_refs, write_bench_profile_json, BenchRun,
    };
    use std::path::PathBuf;

    fn bench_run(iteration: usize, wall_time_ms: f64) -> BenchRun {
        BenchRun {
            iteration,
            wall_time_ms,
            llm_time_ms: 0,
            input_tokens: 0,
            output_tokens: 0,
            call_count: 0,
            total_cost_usd: 0.0,
            profile: None,
        }
    }

    #[test]
    fn bench_report_summarizes_runs() {
        let report = render_bench_report(
            "examples/demo.harn",
            &[
                BenchRun {
                    llm_time_ms: 4,
                    input_tokens: 100,
                    output_tokens: 40,
                    call_count: 1,
                    total_cost_usd: 0.002,
                    ..bench_run(1, 10.0)
                },
                BenchRun {
                    llm_time_ms: 6,
                    input_tokens: 120,
                    output_tokens: 50,
                    call_count: 2,
                    total_cost_usd: 0.003,
                    ..bench_run(2, 14.0)
                },
            ],
            false,
            None,
        );

        assert!(report.contains("Benchmark: examples/demo.harn"));
        assert!(report.contains("Iterations: 2"));
        assert!(report.contains("mean 12.00 ms"));
        assert!(report.contains("p50 12.00 ms"));
        assert!(report.contains("p95 13.80 ms"));
        assert!(report.contains("stddev 2.00 ms"));
        assert!(report.contains("LLM calls: total 3 | avg 1.50/run"));
        assert!(report.contains("Cost: total $0.0050 | avg $0.0025/run"));
    }

    #[test]
    fn bench_stats_reports_percentiles_and_stddev() {
        let runs = [10.0, 20.0, 30.0, 40.0, 50.0]
            .into_iter()
            .enumerate()
            .map(|(index, wall_time_ms)| bench_run(index + 1, wall_time_ms))
            .collect::<Vec<_>>();

        let stats = bench_stats(&runs);

        assert_eq!(stats.mean_ms, 30.0);
        assert_eq!(stats.p50_ms, 30.0);
        assert_eq!(stats.p95_ms, 48.0);
        assert_eq!(percentile_sorted(&[10.0, 20.0], 0.50), 15.0);
        assert!((stats.stddev_ms - 14.1421356237).abs() < 0.0001);
    }

    #[test]
    fn bench_profile_json_includes_iterations_stats_and_rollup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bench.json");
        let runs = vec![bench_run(1, 10.0), bench_run(2, 14.0)];
        let rollup = harn_vm::profile::build(&[]);

        write_bench_profile_json(&path, "examples/demo.harn", &runs, Some(&rollup))
            .expect("write benchmark profile json");

        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("read benchmark json"))
                .expect("benchmark json");
        assert_eq!(value["path"], "examples/demo.harn");
        assert_eq!(value["iterations"].as_array().unwrap().len(), 2);
        assert_eq!(value["iterations"][0]["iteration"], 1);
        assert_eq!(value["mean_ms"], 12.0);
        assert_eq!(value["p50_ms"], 12.0);
        assert_eq!(value["p95_ms"], 13.8);
        assert_eq!(value["stddev_ms"], 2.0);
        assert!(value["rollup"]["by_kind"].is_array());
    }

    #[test]
    fn replay_benchmark_suite_manifest_runs_canonical_fixtures() {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|path| path.parent())
            .expect("repo root")
            .to_path_buf();
        let selection = repo_root.join("benchmarks/replay/suite.json");
        let (suite_name, fixture_paths) =
            resolve_replay_benchmark_selection(&selection, &repo_root, "fallback")
                .expect("resolve replay benchmark suite");
        assert_eq!(suite_name, "harn-canonical-replay-determinism");
        assert_eq!(fixture_paths.len(), 4);

        let reports = fixture_paths
            .iter()
            .map(|path| {
                let trace = read_replay_trace_fixture(path).expect("read replay fixture");
                validate_protocol_fixture_refs(&repo_root, &trace.protocol_fixture_refs)
                    .expect("protocol refs valid");
                harn_vm::benchmark_replay_trace(path.to_string_lossy(), &trace)
                    .expect("benchmark replay trace")
            })
            .collect::<Vec<_>>();
        let report = harn_vm::build_replay_benchmark_report(
            suite_name,
            reports.iter().map(|fixture| fixture.path.clone()).collect(),
            reports,
        );

        assert_eq!(report.summary.passed, 4);
        assert_eq!(report.summary.failed, 0);
        assert_eq!(report.summary.deterministic_fixtures, 4);
        assert_eq!(report.summary.tool_call_drift_count, 0);
        assert_eq!(report.summary.transcript_drift_count, 0);
        assert_eq!(report.summary.mean_replay_fidelity_score, 1.0);
        assert_eq!(
            report.summary.mean_permission_decision_preservation_score,
            1.0
        );
        let text = render_replay_benchmark_report(&report);
        assert!(text.contains("Replay benchmark: harn-canonical-replay-determinism"));
        assert!(text.contains("PASS simple_tool_run"));
        assert!(text.contains("PASS composition_readonly"));
    }
}
