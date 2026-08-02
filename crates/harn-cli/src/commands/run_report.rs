//! User-facing `harn runs report` adapter.
//!
//! Harn VM owns report assembly; this module only adapts CLI paths and output.

use std::path::PathBuf;

use crate::cli::RunsReportArgs;

pub(crate) async fn run(args: RunsReportArgs) -> i32 {
    let path = PathBuf::from(args.path);
    let source_root = path.parent().map(PathBuf::from);
    let request = harn_vm::orchestration::RunReportRequest {
        run_record_path: path,
        events_db: args.events_db,
        source_root,
        ..harn_vm::orchestration::RunReportRequest::default()
    };
    match harn_vm::orchestration::build_run_report(request).await {
        Ok(report) => match serde_json::to_string_pretty(&report) {
            Ok(rendered) => {
                println!("{rendered}");
                0
            }
            Err(error) => {
                eprintln!("error: failed to render run report: {error}");
                1
            }
        },
        Err(error) => {
            eprintln!("error: failed to build run report: {error}");
            1
        }
    }
}
