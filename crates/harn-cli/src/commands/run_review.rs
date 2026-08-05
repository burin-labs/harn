//! User-facing `harn runs review` adapter.

use std::path::PathBuf;

use crate::cli::RunsReviewArgs;

pub(crate) async fn run(args: RunsReviewArgs) -> i32 {
    let rubric = match args.rubric {
        Some(path) => match std::fs::read_to_string(&path) {
            Ok(rubric) => rubric,
            Err(error) => {
                eprintln!("error: failed to read rubric {}: {error}", path.display());
                return 1;
            }
        },
        None => harn_vm::orchestration::DEFAULT_RUN_REVIEW_RUBRIC.to_string(),
    };
    // A projected session yields a run record, so it joins the `--run-record`
    // arm rather than becoming a third kind of review input. Clap's
    // `review_input` group already guarantees exactly one of the three was
    // given; this only decides which arm a `--from-session` lands in.
    let run_record = match args.source.from_session {
        Some(_) => Some(PathBuf::from(
            crate::cli::resolve_run_path_or_exit(None, &args.source).await,
        )),
        None => args.run_record,
    };
    let input = match (args.report, run_record) {
        (Some(path), None) => harn_vm::orchestration::RunReviewInput::Report {
            path,
            allowed_roots: Vec::new(),
        },
        (None, Some(run_record_path)) => harn_vm::orchestration::RunReviewInput::RunRecord(
            super::run_report::request_for_run_record(run_record_path, args.events_db),
        ),
        _ => {
            eprintln!(
                "error: exactly one of --report, --run-record, or --from-session is required"
            );
            return 2;
        }
    };
    let request = harn_vm::orchestration::RunReviewRequest {
        input,
        rubric,
        model: args.model,
    };
    match harn_vm::orchestration::review_run_report(request).await {
        Ok(review) => render(&review, false),
        Err(error) => render(&error, true),
    }
}

fn render(value: &impl serde::Serialize, stderr: bool) -> i32 {
    match serde_json::to_string_pretty(value) {
        Ok(rendered) => {
            if stderr {
                eprintln!("{rendered}");
                1
            } else {
                println!("{rendered}");
                0
            }
        }
        Err(error) => {
            eprintln!("error: failed to render run review: {error}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_vm::orchestration::{
        run_report_projection_hash, RunReport, RunReportProjection, RUN_REPORT_SCHEMA,
        RUN_REPORT_SCHEMA_VERSION,
    };

    #[tokio::test(flavor = "current_thread")]
    async fn cli_review_runs_one_mocked_structured_call() {
        harn_vm::llm::clear_cli_llm_mock_mode();
        let dir = tempfile::tempdir().expect("tempdir");
        let report_path = dir.path().join("report.json");
        let mut report = RunReport {
            schema: RUN_REPORT_SCHEMA.to_string(),
            schema_version: RUN_REPORT_SCHEMA_VERSION,
            projection: RunReportProjection {
                id: "run_report:cli-smoke".to_string(),
                hash: String::new(),
            },
            root_run_id: "cli-smoke".to_string(),
            ..RunReport::default()
        };
        report.projection.hash = run_report_projection_hash(&report).expect("hash");
        std::fs::write(
            &report_path,
            serde_json::to_vec_pretty(&report).expect("report JSON"),
        )
        .expect("write report");
        let fixture = harn_vm::llm::parse_llm_mocks_jsonl(
            &serde_json::json!({
                "provider": "openai",
                "model": "gpt-5.6-luna",
                "text": "{\"verdict\":\"pass\",\"confidence\":0.95,\"summary\":\"The report has no structural failures.\",\"findings\":[],\"actions\":[]}"
            })
            .to_string(),
        )
        .expect("fixture");
        harn_vm::llm::install_cli_llm_mock_fixture(fixture);

        let code = run(RunsReviewArgs {
            report: Some(report_path),
            run_record: None,
            source: Default::default(),
            events_db: None,
            rubric: None,
            model: Some("gpt-5.6-luna".to_string()),
        })
        .await;
        harn_vm::llm::clear_cli_llm_mock_mode();
        assert_eq!(code, 0);
    }
}
