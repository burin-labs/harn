//! `harn merge-captain` — driver and JSONL transcript oracle CLIs.
//!
//! Wraps `harn_vm::orchestration` so CI gates and local eval loops can
//! run a deterministic Merge Captain sweep and audit the resulting
//! transcript either as machine-readable JSON or a human report.

use std::path::{Path, PathBuf};

use harn_vm::orchestration::{
    audit_transcript, load_merge_captain_golden, load_transcript_jsonl, AuditReport,
    MergeCaptainDriverBackend, MergeCaptainDriverMode, MergeCaptainDriverOptions,
    MergeCaptainGolden,
};
use harn_vm::value::VmError;

use crate::cli::{
    MergeCaptainAuditArgs, MergeCaptainAuditFormat, MergeCaptainBackendKind, MergeCaptainRunArgs,
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
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create summary directory {}: {error}",
                parent.display()
            )
        })?;
    }
    let mut bytes = serde_json::to_vec_pretty(summary)
        .map_err(|error| format!("failed to serialize merge-captain summary: {error}"))?;
    bytes.push(b'\n');
    std::fs::write(path, bytes)
        .map_err(|error| format!("failed to write summary {}: {error}", path.display()))
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
    match serde_json::to_string_pretty(report) {
        Ok(text) => println!("{}", text),
        Err(error) => {
            eprintln!("error: failed to serialize audit report: {error}");
        }
    }
}
