//! `harn merge-captain audit` — JSONL transcript oracle CLI (#1013).
//!
//! Wraps `harn_vm::orchestration::audit_transcript` so CI gates can
//! consume the audit either as machine-readable JSON or a human
//! report.

use std::path::Path;

use harn_vm::orchestration::{
    audit_transcript, load_merge_captain_golden, load_transcript_jsonl, AuditReport,
    MergeCaptainGolden,
};
use harn_vm::value::VmError;

use crate::cli::{MergeCaptainAuditArgs, MergeCaptainAuditFormat};

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
