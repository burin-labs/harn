//! `harn merge-captain` — driver and JSONL transcript oracle CLIs.
//!
//! Wraps `harn_vm::orchestration` so CI gates and local eval loops can
//! run a deterministic Merge Captain sweep and audit the resulting
//! transcript either as machine-readable JSON or a human report.

use std::path::{Path, PathBuf};

use harn_vm::orchestration::{
    audit_transcript, diff_merge_captain_iterations, load_eval_pack_manifest,
    load_merge_captain_golden, load_merge_captain_iteration_manifest, load_transcript_jsonl,
    render_iteration_diff_markdown, render_iteration_markdown, AuditReport,
    MergeCaptainDriverBackend, MergeCaptainDriverMode, MergeCaptainDriverOptions,
    MergeCaptainGolden, PersonaEvalLadderManifest, PersonaEvalLadderReport,
};
use harn_vm::value::VmError;

use crate::cli::{
    MergeCaptainAuditArgs, MergeCaptainAuditFormat, MergeCaptainBackendKind,
    MergeCaptainIterateArgs, MergeCaptainIterateFormat, MergeCaptainLadderArgs,
    MergeCaptainLadderFormat, MergeCaptainRunArgs,
};

pub(crate) fn run_driver(args: &MergeCaptainRunArgs) -> i32 {
    let backend = match resolve_backend(args) {
        Ok(backend) => backend,
        Err(message) => {
            eprintln!("error: {message}");
            return 2;
        }
    };
    let mode = if args.watch {
        MergeCaptainDriverMode::Watch
    } else {
        MergeCaptainDriverMode::Once
    };
    let stream_stdout = !args.no_stdout && args.transcript_out.is_none();
    let options = MergeCaptainDriverOptions {
        backend,
        mode,
        model_route: args.model_route.clone(),
        timeout_tier: args.timeout_tier.clone(),
        transcript_out: args.transcript_out.as_deref().map(PathBuf::from),
        receipt_out: args.receipt_out.as_deref().map(PathBuf::from),
        run_root: default_run_dir(),
        max_sweeps: args.max_sweeps,
        watch_backoff_ms: args.watch_backoff_ms,
        stream_stdout,
    };

    let output = match harn_vm::orchestration::run_merge_captain_driver(options) {
        Ok(output) => output,
        Err(VmError::Runtime(message)) => {
            eprintln!("error: {message}");
            return 1;
        }
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };

    match &args.summary_out {
        Some(path) => {
            if let Err(error) = write_summary(Path::new(path), &output.summary) {
                eprintln!("error: {error}");
                return 1;
            }
        }
        None if stream_stdout => match serde_json::to_string(&output.summary) {
            Ok(summary) => eprintln!("{summary}"),
            Err(error) => {
                eprintln!("error: failed to serialize merge-captain summary: {error}");
                return 1;
            }
        },
        None => match serde_json::to_string_pretty(&output.summary) {
            Ok(summary) => println!("{summary}"),
            Err(error) => {
                eprintln!("error: failed to serialize merge-captain summary: {error}");
                return 1;
            }
        },
    }

    if output.summary.pass {
        0
    } else {
        1
    }
}

pub(crate) fn run_ladder(args: &MergeCaptainLadderArgs) -> i32 {
    let manifest_path = Path::new(&args.manifest);
    let manifest = match load_ladder_manifest_for_cli(manifest_path) {
        Ok(manifest) => manifest,
        Err(VmError::Runtime(message)) => {
            eprintln!("error: {message}");
            return 1;
        }
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };

    let report = match harn_vm::orchestration::run_persona_eval_ladder(&manifest) {
        Ok(report) => report,
        Err(VmError::Runtime(message)) => {
            eprintln!("error: {message}");
            return 1;
        }
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };

    if let Some(path) = &args.report_out {
        if let Err(error) = write_json(Path::new(path), &report, "merge-captain ladder report") {
            eprintln!("error: {error}");
            return 1;
        }
    }

    match args.format {
        MergeCaptainLadderFormat::Json => print_json_value(&report),
        MergeCaptainLadderFormat::Text => print_ladder_report(&report),
    }

    if report.pass {
        0
    } else {
        1
    }
}

pub(crate) fn run_iterate(args: &MergeCaptainIterateArgs) -> i32 {
    if !args.diff.is_empty() {
        return run_iteration_diff(args);
    }

    let Some(manifest) = args.manifest.as_deref() else {
        eprintln!("error: merge-captain iterate requires a manifest or --diff A B");
        return 2;
    };
    let manifest_path = Path::new(manifest);
    let manifest = match load_merge_captain_iteration_manifest(manifest_path) {
        Ok(manifest) => manifest,
        Err(VmError::Runtime(message)) => {
            eprintln!("error: {message}");
            return 1;
        }
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    let report = match harn_vm::orchestration::run_merge_captain_iteration(&manifest) {
        Ok(report) => report,
        Err(VmError::Runtime(message)) => {
            eprintln!("error: {message}");
            return 1;
        }
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };

    if let Some(path) = &args.report_out {
        if let Err(error) = write_json(Path::new(path), &report, "merge-captain iteration report") {
            eprintln!("error: {error}");
            return 1;
        }
    }
    let markdown = render_iteration_markdown(&report);
    if let Some(path) = &args.markdown_out {
        if let Err(error) = write_text(
            Path::new(path),
            &markdown,
            "merge-captain iteration Markdown summary",
        ) {
            eprintln!("error: {error}");
            return 1;
        }
    }

    match args.format {
        MergeCaptainIterateFormat::Json => print_json_value(&report),
        MergeCaptainIterateFormat::Text => print!("{markdown}"),
    }

    if report.budget_exhausted || report.completed == 0 {
        1
    } else {
        0
    }
}

fn run_iteration_diff(args: &MergeCaptainIterateArgs) -> i32 {
    if args.diff.len() != 2 {
        eprintln!("error: --diff requires exactly two paths");
        return 2;
    }
    let diff =
        match diff_merge_captain_iterations(Path::new(&args.diff[0]), Path::new(&args.diff[1])) {
            Ok(diff) => diff,
            Err(VmError::Runtime(message)) => {
                eprintln!("error: {message}");
                return 1;
            }
            Err(error) => {
                eprintln!("error: {error}");
                return 1;
            }
        };
    if let Some(path) = &args.report_out {
        if let Err(error) = write_json(
            Path::new(path),
            &diff,
            "merge-captain iteration diff report",
        ) {
            eprintln!("error: {error}");
            return 1;
        }
    }
    let markdown = render_iteration_diff_markdown(&diff);
    if let Some(path) = &args.markdown_out {
        if let Err(error) = write_text(
            Path::new(path),
            &markdown,
            "merge-captain iteration Markdown diff",
        ) {
            eprintln!("error: {error}");
            return 1;
        }
    }
    match args.format {
        MergeCaptainIterateFormat::Json => print_json_value(&diff),
        MergeCaptainIterateFormat::Text => print!("{markdown}"),
    }
    0
}

fn load_ladder_manifest_for_cli(path: &Path) -> Result<PersonaEvalLadderManifest, VmError> {
    if let Some(ladder) = load_single_ladder_from_eval_pack(path)? {
        return Ok(ladder);
    }
    harn_vm::orchestration::load_persona_eval_ladder_manifest(path)
}

fn load_single_ladder_from_eval_pack(
    path: &Path,
) -> Result<Option<PersonaEvalLadderManifest>, VmError> {
    let Ok(pack) = load_eval_pack_manifest(path) else {
        return Ok(None);
    };
    if pack.ladders.is_empty() {
        return Ok(None);
    }
    if pack.ladders.len() != 1 {
        return Err(VmError::Runtime(format!(
            "eval pack {} contains {} persona ladders; use `harn eval` for multi-ladder packs or pass a standalone ladder manifest",
            path.display(),
            pack.ladders.len()
        )));
    }
    let mut ladder = pack.ladders.into_iter().next().expect("checked one ladder");
    if ladder.base_dir.is_none() {
        ladder.base_dir = pack.base_dir;
    }
    Ok(Some(ladder))
}

fn default_run_dir() -> PathBuf {
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    harn_vm::runtime_paths::run_root(&base)
}

fn resolve_backend(args: &MergeCaptainRunArgs) -> Result<MergeCaptainDriverBackend, String> {
    match args.backend {
        MergeCaptainBackendKind::Live => {
            if args.backend_arg.is_some() {
                return Err("--backend live does not accept BACKEND_ARG".to_string());
            }
            Ok(MergeCaptainDriverBackend::Live)
        }
        MergeCaptainBackendKind::Mock => {
            let path = args.backend_arg.as_deref().ok_or_else(|| {
                "--backend mock requires BACKEND_ARG playground directory".to_string()
            })?;
            Ok(MergeCaptainDriverBackend::Mock {
                playground_dir: PathBuf::from(path),
            })
        }
        MergeCaptainBackendKind::Replay => {
            let path = args.backend_arg.as_deref().ok_or_else(|| {
                "--backend replay requires BACKEND_ARG transcript fixture".to_string()
            })?;
            Ok(MergeCaptainDriverBackend::Replay {
                fixture: PathBuf::from(path),
            })
        }
    }
}

fn write_summary(
    path: &Path,
    summary: &harn_vm::orchestration::MergeCaptainRunSummary,
) -> Result<(), String> {
    write_json(path, summary, "merge-captain summary")
}

fn write_json<T: serde::Serialize>(path: &Path, value: &T, label: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create {label} directory {}: {error}",
                parent.display()
            )
        })?;
    }
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("failed to serialize {label}: {error}"))?;
    bytes.push(b'\n');
    std::fs::write(path, bytes)
        .map_err(|error| format!("failed to write {label} {}: {error}", path.display()))
}

fn write_text(path: &Path, value: &str, label: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create {label} directory {}: {error}",
                parent.display()
            )
        })?;
    }
    std::fs::write(path, value)
        .map_err(|error| format!("failed to write {label} {}: {error}", path.display()))
}

fn print_ladder_report(report: &PersonaEvalLadderReport) {
    println!(
        "{} ladder={} persona={} first_correct={}/{} artifacts={}",
        if report.pass { "PASS" } else { "FAIL" },
        report.id,
        report.persona,
        report.first_correct_route.as_deref().unwrap_or("<none>"),
        report.first_correct_tier.as_deref().unwrap_or("<none>"),
        report.artifact_root
    );
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
        if !tier.degradation_reasons.is_empty() {
            for reason in &tier.degradation_reasons {
                println!("  {}", reason);
            }
        }
        println!(
            "  transcript: {}",
            tier.transcript_path.as_deref().unwrap_or("-")
        );
        println!("  receipt: {}", tier.receipt_path);
    }
}

pub(crate) fn run_audit(args: &MergeCaptainAuditArgs) -> i32 {
    let transcript_path = Path::new(&args.transcript);
    let loaded = match load_transcript_jsonl(transcript_path) {
        Ok(loaded) => loaded,
        Err(VmError::Runtime(message)) => {
            eprintln!("error: {message}");
            return 1;
        }
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };

    let golden: Option<MergeCaptainGolden> = match args.golden.as_deref() {
        Some(path) => match load_merge_captain_golden(Path::new(path)) {
            Ok(golden) => Some(golden),
            Err(VmError::Runtime(message)) => {
                eprintln!("error: {message}");
                return 1;
            }
            Err(error) => {
                eprintln!("error: {error}");
                return 1;
            }
        },
        None => None,
    };

    let mut report = audit_transcript(&loaded.events, golden.as_ref());
    report.source_path = Some(loaded.source_path.display().to_string());

    match args.format {
        MergeCaptainAuditFormat::Json => {
            print_json(&report);
        }
        MergeCaptainAuditFormat::Text => {
            print!("{}", report);
        }
    }

    let strict_warnings_failed = args.strict && report.warn_findings() > 0;
    if !report.pass || strict_warnings_failed {
        return 1;
    }
    0
}

fn print_json(report: &AuditReport) {
    print_json_value(report);
}

fn print_json_value<T: serde::Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(text) => println!("{}", text),
        Err(error) => {
            eprintln!("error: failed to serialize JSON output: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::load_ladder_manifest_for_cli;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    fn write_pack(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn ladder_cli_accepts_single_ladder_eval_pack() {
        let temp = tempfile::tempdir().unwrap();
        let manifest_path = temp.path().join("harn.eval.toml");
        write_pack(
            &manifest_path,
            &format!(
                r#"
version = 1
id = "merge-captain-ladders"
base_dir = "{}"

[[ladders]]
id = "green-pr-value-model"
persona = "merge_captain"
artifact-root = "{}"

[ladders.backend]
kind = "replay"
path = "examples/personas/merge_captain/transcripts/green_pr.jsonl"

[[ladders.model-routes]]
id = "gemma-value"
route = "local/gemma-value"

[[ladders.timeout-tiers]]
id = "balanced"
max-tool-calls = 4
"#,
                repo_root().display(),
                temp.path().join("artifacts").display()
            ),
        );

        let manifest = load_ladder_manifest_for_cli(&manifest_path).unwrap();

        assert_eq!(manifest.id, "green-pr-value-model");
        assert_eq!(
            manifest.base_dir.as_deref(),
            Some(repo_root().to_str().unwrap())
        );
        assert_eq!(manifest.timeout_tiers[0].id, "balanced");
    }

    #[test]
    fn ladder_cli_rejects_multi_ladder_eval_pack() {
        let temp = tempfile::tempdir().unwrap();
        let manifest_path = temp.path().join("harn.eval.toml");
        write_pack(
            &manifest_path,
            r#"
version = 1
id = "merge-captain-ladders"

[[ladders]]
id = "one"

[[ladders.timeout-tiers]]
id = "balanced"

[[ladders]]
id = "two"

[[ladders.timeout-tiers]]
id = "balanced"
"#,
        );

        let error = load_ladder_manifest_for_cli(&manifest_path).unwrap_err();

        assert!(format!("{error}").contains("contains 2 persona ladders"));
    }
}
