use std::path::Path;
use std::process;
use std::time::Instant;

use harn_parser::DiagnosticSeverity;

use crate::commands::run::connect_mcp_servers;
use crate::package;
use crate::parse_source_file;

#[derive(Debug, Clone, Copy)]
struct BenchRun {
    wall_time_ms: f64,
    llm_time_ms: i64,
    input_tokens: i64,
    output_tokens: i64,
    call_count: i64,
    total_cost_usd: f64,
}

pub(crate) async fn run_bench(path: &str, iterations: usize) {
    if iterations == 0 {
        eprintln!("error: `harn bench` requires at least one iteration");
        process::exit(1);
    }

    let (source, program) = parse_source_file(path);
    let type_diagnostics = harn_parser::TypeChecker::new().check(&program);
    let mut had_type_error = false;
    for diag in &type_diagnostics {
        match diag.severity {
            DiagnosticSeverity::Error => {
                had_type_error = true;
                if let Some(span) = &diag.span {
                    let rendered = harn_parser::diagnostic::render_diagnostic(
                        &source,
                        path,
                        span,
                        "error",
                        &diag.message,
                        None,
                        diag.help.as_deref(),
                    );
                    eprint!("{rendered}");
                } else {
                    eprintln!("error: {}", diag.message);
                }
            }
            DiagnosticSeverity::Warning => {
                if let Some(span) = &diag.span {
                    let rendered = harn_parser::diagnostic::render_diagnostic(
                        &source,
                        path,
                        span,
                        "warning",
                        &diag.message,
                        None,
                        diag.help.as_deref(),
                    );
                    eprint!("{rendered}");
                } else {
                    eprintln!("warning: {}", diag.message);
                }
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
    let manifest = package::try_read_manifest_for(Path::new(path));

    let mut runs = Vec::with_capacity(iterations);
    for iteration in 0..iterations {
        harn_vm::reset_thread_local_state();
        harn_vm::llm::enable_tracing();

        let mut vm = harn_vm::Vm::new();
        harn_vm::register_vm_stdlib(&mut vm);
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

        if let Some(manifest) = manifest.as_ref() {
            if !manifest.mcp.is_empty() {
                connect_mcp_servers(&manifest.mcp, &mut vm).await;
            }
        }

        let started_at = Instant::now();
        let local = tokio::task::LocalSet::new();
        let execution_result = local.run_until(async { vm.execute(&chunk).await }).await;
        let wall_time_ms = started_at.elapsed().as_secs_f64() * 1000.0;

        match execution_result {
            Ok(_) => {
                let (input_tokens, output_tokens, llm_time_ms, call_count) =
                    harn_vm::llm::peek_trace_summary();
                runs.push(BenchRun {
                    wall_time_ms,
                    llm_time_ms,
                    input_tokens,
                    output_tokens,
                    call_count,
                    total_cost_usd: harn_vm::llm::peek_total_cost(),
                });
            }
            Err(error) => {
                eprint!("{}", vm.format_runtime_error(&error));
                eprintln!("benchmark aborted on iteration {}", iteration + 1);
                process::exit(1);
            }
        }
    }

    print!("{}", render_bench_report(path, &runs));
}

fn render_bench_report(path: &str, runs: &[BenchRun]) -> String {
    let iterations = runs.len() as f64;
    let total_wall = runs.iter().map(|run| run.wall_time_ms).sum::<f64>();
    let total_llm = runs.iter().map(|run| run.llm_time_ms).sum::<i64>();
    let total_input = runs.iter().map(|run| run.input_tokens).sum::<i64>();
    let total_output = runs.iter().map(|run| run.output_tokens).sum::<i64>();
    let total_calls = runs.iter().map(|run| run.call_count).sum::<i64>();
    let total_cost = runs.iter().map(|run| run.total_cost_usd).sum::<f64>();
    let min_wall = runs
        .iter()
        .map(|run| run.wall_time_ms)
        .fold(f64::INFINITY, f64::min);
    let max_wall = runs
        .iter()
        .map(|run| run.wall_time_ms)
        .fold(f64::NEG_INFINITY, f64::max);

    format!(
        "\
Benchmark: {path}
Iterations: {}
Wall time: min {:.2} ms | avg {:.2} ms | max {:.2} ms | total {:.2} ms
LLM time: total {} ms | avg {:.2} ms/run
LLM calls: total {} | avg {:.2}/run
Input tokens: total {} | avg {:.2}/run
Output tokens: total {} | avg {:.2}/run
Cost: total ${:.4} | avg ${:.4}/run
",
        runs.len(),
        min_wall,
        total_wall / iterations,
        max_wall,
        total_wall,
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
    )
}

#[cfg(test)]
mod tests {
    use super::{render_bench_report, BenchRun};

    #[test]
    fn bench_report_summarizes_runs() {
        let report = render_bench_report(
            "examples/demo.harn",
            &[
                BenchRun {
                    wall_time_ms: 10.0,
                    llm_time_ms: 4,
                    input_tokens: 100,
                    output_tokens: 40,
                    call_count: 1,
                    total_cost_usd: 0.002,
                },
                BenchRun {
                    wall_time_ms: 14.0,
                    llm_time_ms: 6,
                    input_tokens: 120,
                    output_tokens: 50,
                    call_count: 2,
                    total_cost_usd: 0.003,
                },
            ],
        );

        assert!(report.contains("Benchmark: examples/demo.harn"));
        assert!(report.contains("Iterations: 2"));
        assert!(report.contains("Wall time: min 10.00 ms | avg 12.00 ms | max 14.00 ms"));
        assert!(report.contains("LLM calls: total 3 | avg 1.50/run"));
        assert!(report.contains("Cost: total $0.0050 | avg $0.0025/run"));
    }
}
