use std::path::{Path, PathBuf};

use serde::Serialize;

use super::common::print_json;
use super::errors::OrchestratorError;
use crate::cli::OrchestratorReplayOracleArgs;
use crate::commands::replay_fixtures::{
    collect_json_files, discover_repo_root, display_path, read_trace_fixture, resolve_selection,
    validate_protocol_fixture_refs,
};

#[derive(Debug, Serialize)]
struct ReplayOracleSuiteReport {
    passed: usize,
    failed: usize,
    skipped: usize,
    fixtures: Vec<ReplayOracleFixtureOutcome>,
}

#[derive(Debug, Serialize)]
struct ReplayOracleFixtureOutcome {
    path: String,
    name: Option<String>,
    passed: bool,
    expectation: Option<harn_vm::ReplayExpectation>,
    error: Option<String>,
    divergence: Option<harn_vm::ReplayDivergence>,
    first_run_counts: Option<harn_vm::ReplayTraceRunCounts>,
    second_run_counts: Option<harn_vm::ReplayTraceRunCounts>,
    protocol_fixture_refs: Vec<String>,
}

pub(super) async fn run(args: OrchestratorReplayOracleArgs) -> Result<(), OrchestratorError> {
    let repo_root = discover_repo_root().unwrap_or_else(|| PathBuf::from("."));
    let selection = resolve_selection(
        args.selection.as_deref(),
        &repo_root,
        "conformance/replay-oracle/fixtures",
    );
    let fixtures = resolve_fixture_files(&selection)?;
    let mut report = ReplayOracleSuiteReport {
        passed: 0,
        failed: 0,
        skipped: 0,
        fixtures: Vec::new(),
    };

    for fixture_path in fixtures {
        let display_path = display_path(&fixture_path);
        let trace = match read_trace_fixture(&fixture_path) {
            Ok(trace) => trace,
            Err(error) => {
                report.failed += 1;
                if !args.json {
                    println!("  \x1b[31mFAIL\x1b[0m  {display_path}");
                }
                report.fixtures.push(ReplayOracleFixtureOutcome {
                    path: display_path,
                    name: None,
                    passed: false,
                    expectation: None,
                    error: Some(error),
                    divergence: None,
                    first_run_counts: None,
                    second_run_counts: None,
                    protocol_fixture_refs: Vec::new(),
                });
                continue;
            }
        };

        if !matches_filter(args.filter.as_deref(), &display_path, &trace) {
            report.skipped += 1;
            continue;
        }

        let protocol_ref_error =
            validate_protocol_fixture_refs(&repo_root, &trace.protocol_fixture_refs)
                .err()
                .map(|error| format!("{}: {error}", trace.name));
        let trace_report = match protocol_ref_error {
            Some(error) => Err(error),
            None => harn_vm::run_replay_oracle_trace(&trace).map_err(|error| error.to_string()),
        };

        match trace_report {
            Ok(trace_report) if trace_report.passed => {
                report.passed += 1;
                if !args.json {
                    println!("  \x1b[32mPASS\x1b[0m  {}", trace_report.name);
                }
                report.fixtures.push(ReplayOracleFixtureOutcome {
                    path: display_path,
                    name: Some(trace_report.name),
                    passed: true,
                    expectation: Some(trace_report.expectation),
                    error: None,
                    divergence: trace_report.divergence,
                    first_run_counts: Some(trace_report.first_run_counts),
                    second_run_counts: Some(trace_report.second_run_counts),
                    protocol_fixture_refs: trace_report.protocol_fixture_refs,
                });
            }
            Ok(trace_report) => {
                report.failed += 1;
                if !args.json {
                    println!("  \x1b[31mFAIL\x1b[0m  {}", trace_report.name);
                    if let Some(divergence) = &trace_report.divergence {
                        print_divergence(divergence);
                    } else {
                        println!(
                            "{}",
                            crate::localization::formatted!(
                                "    expected drift, but canonical replay runs matched"
                            )
                        );
                    }
                }
                report.fixtures.push(ReplayOracleFixtureOutcome {
                    path: display_path,
                    name: Some(trace_report.name),
                    passed: false,
                    expectation: Some(trace_report.expectation),
                    error: None,
                    divergence: trace_report.divergence,
                    first_run_counts: Some(trace_report.first_run_counts),
                    second_run_counts: Some(trace_report.second_run_counts),
                    protocol_fixture_refs: trace_report.protocol_fixture_refs,
                });
            }
            Err(error) => {
                report.failed += 1;
                if !args.json {
                    println!("  \x1b[31mFAIL\x1b[0m  {}", trace.name);
                    println!("    {error}");
                }
                report.fixtures.push(ReplayOracleFixtureOutcome {
                    path: display_path,
                    name: Some(trace.name),
                    passed: false,
                    expectation: Some(trace.expect),
                    error: Some(error),
                    divergence: None,
                    first_run_counts: None,
                    second_run_counts: None,
                    protocol_fixture_refs: trace.protocol_fixture_refs,
                });
            }
        }
    }

    if args.json {
        print_json(&report)?;
    }

    if report.failed > 0 {
        return Err(OrchestratorError::Replay(format!(
            "Replay oracle failed: {} passed, {} failed, {} skipped",
            report.passed, report.failed, report.skipped
        )));
    }

    if !args.json {
        println!(
            "{}",
            crate::localization::formatted!(
                "Replay oracle passed: {} passed, {} skipped",
                report.passed,
                report.skipped
            )
        );
    }
    Ok(())
}

fn resolve_fixture_files(selection: &Path) -> Result<Vec<PathBuf>, OrchestratorError> {
    if !selection.exists() {
        return Err(OrchestratorError::Replay(format!(
            "replay oracle target not found: {}",
            selection.display()
        )));
    }
    let mut files = Vec::new();
    collect_json_files(selection, &mut files);
    if files.is_empty() {
        return Err(OrchestratorError::Replay(format!(
            "no replay oracle JSON fixtures found under {}",
            selection.display()
        )));
    }
    Ok(files)
}

fn matches_filter(
    filter: Option<&str>,
    display_path: &str,
    trace: &harn_vm::ReplayOracleTrace,
) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    trace.name.contains(filter)
        || display_path.contains(filter)
        || trace
            .protocol_fixture_refs
            .iter()
            .any(|fixture_ref| fixture_ref.contains(filter))
}

fn print_divergence(divergence: &harn_vm::ReplayDivergence) {
    println!(
        "{}",
        crate::localization::formatted!("    first divergence at {}", divergence.path)
    );
    println!(
        "{}",
        crate::localization::formatted!("    left: {}", compact_json(&divergence.left))
    );
    println!(
        "{}",
        crate::localization::formatted!("    right: {}", compact_json(&divergence.right))
    );
}

fn compact_json(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unprintable>".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_matches_trace_name_path_or_protocol_ref() {
        let trace = harn_vm::ReplayOracleTrace {
            name: "handler_to_a2a_worker".to_string(),
            protocol_fixture_refs: vec![
                "conformance/protocols/fixtures/a2a/task_and_stream.valid.json".to_string(),
            ],
            ..harn_vm::ReplayOracleTrace::default()
        };

        assert!(matches_filter(
            Some("a2a_worker"),
            "fixtures/worker.json",
            &trace
        ));
        assert!(matches_filter(
            Some("worker.json"),
            "fixtures/worker.json",
            &trace
        ));
        assert!(matches_filter(
            Some("task_and_stream"),
            "fixtures/worker.json",
            &trace
        ));
        assert!(!matches_filter(Some("mcp"), "fixtures/worker.json", &trace));
    }

    #[test]
    fn protocol_refs_must_use_checked_in_protocol_matrix() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("conformance/protocols/fixtures/acp")).unwrap();
        std::fs::write(
            temp.path()
                .join("conformance/protocols/fixtures/acp/session.valid.json"),
            "{}",
        )
        .unwrap();

        assert!(validate_protocol_fixture_refs(
            temp.path(),
            &["conformance/protocols/fixtures/acp/session.valid.json".to_string()]
        )
        .is_ok());
        assert!(validate_protocol_fixture_refs(
            temp.path(),
            &["conformance/replay-oracle/not-protocol.json".to_string()]
        )
        .is_err());
        assert!(validate_protocol_fixture_refs(
            temp.path(),
            &["conformance/protocols/fixtures/acp/missing.json".to_string()]
        )
        .is_err());
    }
}
