//! `harn eval context` — deterministic context-engineering mode runner.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use harn_vm::orchestration::{
    context_eval_default_output_dir, evaluate_context_eval_manifest, load_context_eval_manifest,
    ContextEvalReport, ContextEvalRunReport,
};

use crate::cli::EvalContextArgs;

pub fn run(args: EvalContextArgs) -> i32 {
    let manifest = match load_context_eval_manifest(&args.manifest) {
        Ok(manifest) => manifest,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    let report = match evaluate_context_eval_manifest(&manifest) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };

    let output_dir = args.output.unwrap_or_else(context_eval_default_output_dir);
    if let Err(error) = fs::create_dir_all(&output_dir) {
        eprintln!("error: failed to create {}: {error}", output_dir.display());
        return 1;
    }
    if let Err(error) = write_outputs(&output_dir, &report) {
        eprintln!("error: failed to write context eval outputs: {error}");
        return 1;
    }
    eprintln!(
        "wrote {}, {}, and {}",
        output_dir.join("summary.json").display(),
        output_dir.join("per_run.jsonl").display(),
        output_dir.join("summary.md").display()
    );
    if args.json {
        match serde_json::to_string_pretty(&report) {
            Ok(payload) => println!("{payload}"),
            Err(error) => {
                eprintln!("error: failed to serialize context eval summary: {error}");
                return 1;
            }
        }
    } else {
        println!(
            "context eval: {}/{} passed, mean_correctness={:.2}, mean_tool_quality={:.2}",
            report.passed_runs,
            report.total_runs,
            report.aggregate.mean_final_correctness,
            report.aggregate.mean_tool_call_quality
        );
    }

    if report.pass {
        0
    } else {
        1
    }
}

fn write_outputs(output_dir: &Path, report: &ContextEvalReport) -> Result<(), String> {
    write_json(output_dir.join("summary.json"), report)?;
    write_jsonl(output_dir.join("per_run.jsonl"), &report.runs)?;
    fs::write(output_dir.join("summary.md"), render_markdown(report))
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn write_json(path: PathBuf, report: &ContextEvalReport) -> Result<(), String> {
    let payload = serde_json::to_string_pretty(report).map_err(|error| error.to_string())?;
    fs::write(path, payload).map_err(|error| error.to_string())
}

fn write_jsonl(path: PathBuf, runs: &[ContextEvalRunReport]) -> Result<(), String> {
    let mut file = fs::File::create(path).map_err(|error| error.to_string())?;
    for run in runs {
        let line = serde_json::to_string(run).map_err(|error| error.to_string())?;
        file.write_all(line.as_bytes())
            .map_err(|error| error.to_string())?;
        file.write_all(b"\n").map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn render_markdown(report: &ContextEvalReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Context Eval: {}\n\n",
        report
            .manifest_name
            .as_deref()
            .unwrap_or(report.manifest_id.as_str())
    ));
    out.push_str(&format!(
        "- status: {}\n- runs: {}/{} passed\n- mean correctness: {:.4}\n- mean tool quality: {:.4}\n- input tokens: {}\n- output tokens: {}\n- cost USD: {:.6}\n\n",
        if report.pass { "PASS" } else { "FAIL" },
        report.passed_runs,
        report.total_runs,
        report.aggregate.mean_final_correctness,
        report.aggregate.mean_tool_call_quality,
        report.aggregate.total_input_tokens,
        report.aggregate.total_output_tokens,
        report.aggregate.total_cost_usd,
    ));
    out.push_str("| task | mode | pass | correctness | tools | reads before edit | input tokens | compactions | cache key |\n");
    out.push_str("|---|---|---:|---:|---:|---:|---:|---:|---|\n");
    for run in &report.runs {
        out.push_str(&format!(
            "| {} | {} | {} | {:.4} | {:.4} | {} | {} | {} | `{}` |\n",
            escape_md(&run.task_id),
            escape_md(&run.mode_id),
            if run.passed { "yes" } else { "no" },
            run.final_correctness.score,
            run.tool_call_quality.score,
            run.reads_before_first_edit,
            run.input_tokens,
            run.compaction_count,
            run.cache.key,
        ));
    }
    out
}

fn escape_md(value: &str) -> String {
    value.replace('|', "\\|")
}
