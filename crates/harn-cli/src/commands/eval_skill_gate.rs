//! `harn eval skill-gate` - contamination-safe gate reports for skill/guidance candidates.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use harn_vm::orchestration::{
    evaluate_skill_gate_manifest, load_skill_gate_manifest, SkillGateCaseReport, SkillGateReport,
    SkillGateVariantReport,
};

use crate::cli::EvalSkillGateArgs;
use crate::format::escape_md;

pub async fn run(args: EvalSkillGateArgs) -> i32 {
    let manifest = match load_skill_gate_manifest(&args.manifest) {
        Ok(manifest) => manifest,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    let report = match evaluate_skill_gate_manifest(&manifest) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    let output_dir = args.output.unwrap_or_else(|| default_output_dir(&report));
    if let Err(error) = fs::create_dir_all(&output_dir) {
        eprintln!("error: failed to create {}: {error}", output_dir.display());
        return 1;
    }
    if let Err(error) = write_outputs(&output_dir, &report) {
        eprintln!("error: failed to write skill gate outputs: {error}");
        return 1;
    }
    eprintln!(
        "wrote {}, {}, {}, and {}",
        output_dir.join("summary.json").display(),
        output_dir.join("per_case.jsonl").display(),
        output_dir.join("receipt.json").display(),
        output_dir.join("summary.md").display()
    );
    if args.json {
        match serde_json::to_string_pretty(&report) {
            Ok(payload) => println!("{payload}"),
            Err(error) => {
                eprintln!("error: failed to serialize skill gate report: {error}");
                return 1;
            }
        }
    } else {
        println!(
            "skill gate: {} selected={} variants={} included={} excluded={} tamper={}",
            if report.pass { "PASS" } else { "FAIL" },
            report.selected_variant_id.as_deref().unwrap_or("none"),
            report.variants.len(),
            report.included_task_count,
            report.excluded_task_count,
            if report.tamper.pass { "pass" } else { "fail" }
        );
    }
    i32::from(!report.pass)
}

fn default_output_dir(report: &SkillGateReport) -> PathBuf {
    Path::new(".harn-runs")
        .join("skill-gate")
        .join(&report.manifest_id)
}

fn write_outputs(output_dir: &Path, report: &SkillGateReport) -> Result<(), String> {
    write_json(output_dir.join("summary.json"), report)?;
    write_per_case(output_dir.join("per_case.jsonl"), report)?;
    write_json(output_dir.join("receipt.json"), &report.receipt)?;
    fs::write(output_dir.join("summary.md"), render_markdown(report))
        .map_err(|error| error.to_string())
}

fn write_json<T: serde::Serialize>(path: PathBuf, value: &T) -> Result<(), String> {
    let payload = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    fs::write(path, payload).map_err(|error| error.to_string())
}

fn write_per_case(path: PathBuf, report: &SkillGateReport) -> Result<(), String> {
    let mut file = fs::File::create(path).map_err(|error| error.to_string())?;
    for variant in &report.variants {
        for case in &variant.cases {
            let line = serde_json::to_string(&PerCaseLine {
                variant_id: &variant.id,
                accepted: variant.accepted,
                case,
            })
            .map_err(|error| error.to_string())?;
            file.write_all(line.as_bytes())
                .map_err(|error| error.to_string())?;
            file.write_all(b"\n").map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct PerCaseLine<'a> {
    variant_id: &'a str,
    accepted: bool,
    #[serde(flatten)]
    case: &'a SkillGateCaseReport,
}

fn render_markdown(report: &SkillGateReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Skill Gate: {}\n\n", report.manifest_id));
    out.push_str(&format!(
        "- status: {}\n- target model: `{}`\n- selected variant: `{}`\n- included tasks: {}\n- excluded tasks: {}\n- tamper: {}\n- pareto frontier: {}\n\n",
        if report.pass { "PASS" } else { "FAIL" },
        escape_md(&report.target_model.id),
        escape_md(report.selected_variant_id.as_deref().unwrap_or("none")),
        report.included_task_count,
        report.excluded_task_count,
        if report.tamper.pass { "pass" } else { "fail" },
        if report.pareto_frontier.is_empty() {
            "none".to_string()
        } else {
            report.pareto_frontier.join(", ")
        }
    ));
    out.push_str(
        "| variant | decision | lift | gap recovery | regressions | context delta | failures |\n",
    );
    out.push_str("|---|---|---:|---:|---:|---:|---|\n");
    for variant in &report.variants {
        out.push_str(&variant_row(variant));
    }
    if !report.task_safety.is_empty() {
        out.push_str("\n## Held-out Filter\n\n");
        out.push_str("| task | cluster | included | reason |\n");
        out.push_str("|---|---|---:|---|\n");
        for task in &report.task_safety {
            out.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                escape_md(&task.task_id),
                escape_md(&task.cluster),
                if task.included { "yes" } else { "no" },
                escape_md(task.exclusion_reason.as_deref().unwrap_or(""))
            ));
        }
    }
    if !report.tamper.checks.is_empty() {
        out.push_str("\n## Immutable Grader Checks\n\n");
        out.push_str("| path | status | actual sha256 |\n");
        out.push_str("|---|---|---|\n");
        for check in &report.tamper.checks {
            out.push_str(&format!(
                "| {} | {} | `{}` |\n",
                escape_md(&check.path),
                escape_md(&check.status),
                check.actual_sha256.as_deref().unwrap_or("")
            ));
        }
    }
    out
}

fn variant_row(variant: &SkillGateVariantReport) -> String {
    format!(
        "| {} | {} | {:.4} | {:.4} | {}/{} | {} | {} |\n",
        escape_md(&variant.id),
        if variant.accepted {
            "accepted"
        } else {
            "rejected"
        },
        variant.metrics.mean_score_lift,
        variant.metrics.mean_gap_recovery,
        variant.metrics.regression_count,
        variant.metrics.regression_denominator,
        variant.context.delta_tokens,
        escape_md(&variant.failures.join("; "))
    )
}
