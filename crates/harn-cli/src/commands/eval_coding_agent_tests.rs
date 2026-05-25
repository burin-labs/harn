use super::*;

fn format_comparison_for_test(
    fixture_id: &str,
    native_status: &str,
    text_status: &str,
) -> FormatComparison {
    let native_passed = native_status == "passed";
    let text_passed = text_status == "passed";
    FormatComparison {
        fixture_id: fixture_id.to_string(),
        selector: ModelSelector {
            selector: "openrouter:qwen/qwen3-coder".to_string(),
            provider: "openrouter".to_string(),
            model: "qwen/qwen3-coder".to_string(),
        },
        native_run_id: Some(format!("{fixture_id}-native")),
        text_run_id: Some(format!("{fixture_id}-text")),
        native_evidence_path: Some(format!("out/{fixture_id}/native/transcript_events.jsonl")),
        text_evidence_path: Some(format!("out/{fixture_id}/text/transcript_events.jsonl")),
        native_status: Some(native_status.to_string()),
        text_status: Some(text_status.to_string()),
        native_passed: Some(native_passed),
        text_passed: Some(text_passed),
        native_tool_call_count: Some(0),
        text_tool_call_count: Some(0),
        native_rejected_tool_call_count: Some(0),
        text_rejected_tool_call_count: Some(0),
        verifier_match: Some(native_passed == text_passed),
        tool_sequence_match: Some(true),
        rejected_tool_call_delta_text_minus_native: Some(0),
        token_delta_text_minus_native: Some(0),
        iteration_delta_text_minus_native: Some(0),
        equivalent: Some(native_status == text_status && native_passed == text_passed),
        divergence_reasons: Vec::new(),
        evidence_paths: vec![
            format!("out/{fixture_id}/native/transcript_events.jsonl"),
            format!("out/{fixture_id}/text/transcript_events.jsonl"),
        ],
    }
}

#[test]
fn dotenv_parser_strips_export_and_quotes_without_leaking_values() {
    let parsed = parse_env_line("export TOGETHER_API_KEY=\"secret\"")
        .unwrap()
        .unwrap();
    assert_eq!(parsed.0, "TOGETHER_API_KEY");
    assert_eq!(parsed.1, "secret");
    assert!(parse_env_line("# comment").unwrap().is_none());
}

#[test]
fn model_selector_args_rejoin_provider_model_kv_after_clap_delimiter_split() {
    let normalized = normalize_model_selector_args(&[
        "mock:mock".to_string(),
        "provider=openrouter".to_string(),
        "model=qwen/qwen3-coder-flash".to_string(),
        "provider=together".to_string(),
        "model=Qwen/Qwen3-Coder-Next-FP8".to_string(),
    ]);
    assert_eq!(
        normalized,
        vec![
            "mock:mock",
            "provider=openrouter,model=qwen/qwen3-coder-flash",
            "provider=together,model=Qwen/Qwen3-Coder-Next-FP8",
        ]
    );
}

#[test]
fn markdown_escapes_model_table_pipes() {
    let selector = ModelSelector {
        selector: "provider:a|b".to_string(),
        provider: "provider".to_string(),
        model: "a|b".to_string(),
    };
    let summary = EvalSummary {
        schema_version: 3,
        fixture_ids: vec!["python-add".to_string()],
        fixtures: vec![FixtureReport {
            id: "python-add".to_string(),
            name: "Python add repair".to_string(),
            tool_sequence: "multi-tool".to_string(),
            description: "One-file Python bug fix verified by unittest output.".to_string(),
        }],
        output_dir: "out".to_string(),
        models: vec![selector.clone()],
        tool_formats: vec!["native".to_string()],
        env_keys_loaded: Vec::new(),
        total_runs: 1,
        passed_runs: 1,
        failed_runs: 0,
        skipped_runs: 0,
        diverged_comparisons: 0,
        total_cost_usd: 0.0,
        rollups: EvalRollups {
            by_fixture: vec![RollupReport {
                key: "python-add".to_string(),
                total_runs: 1,
                passed_runs: 1,
                failed_runs: 0,
                skipped_runs: 0,
                total_cost_usd: 0.0,
            }],
            by_provider: Vec::new(),
            by_model: Vec::new(),
            by_tool_format: Vec::new(),
            by_tool_sequence: Vec::new(),
        },
        runs: vec![RunReport {
            run_id: "r".to_string(),
            fixture_id: "python-add".to_string(),
            fixture_name: "Python add repair".to_string(),
            fixture_tool_sequence: "multi-tool".to_string(),
            selector,
            tool_format: "native".to_string(),
            status: "passed".to_string(),
            passed: true,
            skipped: false,
            skipped_reason: None,
            output_dir: "out/r".to_string(),
            transcript_events_path: "out/r/transcript_events.jsonl".to_string(),
            workspace_root: None,
            elapsed_ms: 1,
            duration_ms: 1,
            iterations: 1,
            input_tokens: 1,
            output_tokens: 1,
            cost_usd: 0.0,
            pricing_known: false,
            tool_calls: 0,
            rejected_tool_calls: 0,
            tool_sequence: Vec::new(),
            successful_tools: Vec::new(),
            transcript_event_count: 0,
            verification_success: true,
            harn_exit_code: 0,
            error: None,
            stderr_excerpt: None,
            local_cleanup: None,
        }],
        comparisons: Vec::new(),
        parity_by_pair: Vec::new(),
        followups: Vec::new(),
        step_judge_preset: None,
        run_label: String::new(),
        baseline_comparison: None,
    };
    let md = render_markdown(&summary);
    assert!(md.contains("a\\|b"));
}

#[test]
fn write_json_artifacts_emits_tool_mode_parity_overlay() {
    let selector = ModelSelector {
        selector: "openrouter:qwen/qwen3-coder".to_string(),
        provider: "openrouter".to_string(),
        model: "qwen/qwen3-coder".to_string(),
    };
    let summary = EvalSummary {
        schema_version: 3,
        fixture_ids: vec!["python-add".to_string()],
        fixtures: vec![FixtureReport {
            id: "python-add".to_string(),
            name: "Python add repair".to_string(),
            tool_sequence: "multi-tool".to_string(),
            description: "One-file Python bug fix verified by unittest output.".to_string(),
        }],
        output_dir: "out".to_string(),
        models: vec![selector.clone()],
        tool_formats: vec!["native".to_string(), "text".to_string()],
        env_keys_loaded: Vec::new(),
        total_runs: 2,
        passed_runs: 1,
        failed_runs: 1,
        skipped_runs: 0,
        diverged_comparisons: 1,
        total_cost_usd: 0.0,
        rollups: EvalRollups {
            by_fixture: Vec::new(),
            by_provider: Vec::new(),
            by_model: Vec::new(),
            by_tool_format: Vec::new(),
            by_tool_sequence: Vec::new(),
        },
        runs: vec![
            RunReport {
                run_id: "native".to_string(),
                fixture_id: "python-add".to_string(),
                fixture_name: "Python add repair".to_string(),
                fixture_tool_sequence: "multi-tool".to_string(),
                selector: selector.clone(),
                tool_format: "native".to_string(),
                status: "failed".to_string(),
                passed: false,
                skipped: false,
                skipped_reason: None,
                output_dir: "out/native".to_string(),
                transcript_events_path: "out/native/transcript_events.jsonl".to_string(),
                workspace_root: None,
                elapsed_ms: 1,
                duration_ms: 1,
                iterations: 1,
                input_tokens: 1,
                output_tokens: 1,
                cost_usd: 0.0,
                pricing_known: false,
                tool_calls: 0,
                rejected_tool_calls: 0,
                tool_sequence: Vec::new(),
                successful_tools: Vec::new(),
                transcript_event_count: 0,
                verification_success: false,
                harn_exit_code: 1,
                error: None,
                stderr_excerpt: None,
                local_cleanup: None,
            },
            RunReport {
                run_id: "text".to_string(),
                fixture_id: "python-add".to_string(),
                fixture_name: "Python add repair".to_string(),
                fixture_tool_sequence: "multi-tool".to_string(),
                selector,
                tool_format: "text".to_string(),
                status: "passed".to_string(),
                passed: true,
                skipped: false,
                skipped_reason: None,
                output_dir: "out/text".to_string(),
                transcript_events_path: "out/text/transcript_events.jsonl".to_string(),
                workspace_root: None,
                elapsed_ms: 1,
                duration_ms: 1,
                iterations: 1,
                input_tokens: 1,
                output_tokens: 1,
                cost_usd: 0.0,
                pricing_known: false,
                tool_calls: 0,
                rejected_tool_calls: 0,
                tool_sequence: Vec::new(),
                successful_tools: Vec::new(),
                transcript_event_count: 0,
                verification_success: true,
                harn_exit_code: 0,
                error: None,
                stderr_excerpt: None,
                local_cleanup: None,
            },
        ],
        comparisons: vec![FormatComparison {
            fixture_id: "python-add".to_string(),
            selector: ModelSelector {
                selector: "openrouter:qwen/qwen3-coder".to_string(),
                provider: "openrouter".to_string(),
                model: "qwen/qwen3-coder".to_string(),
            },
            native_run_id: Some("native".to_string()),
            text_run_id: Some("text".to_string()),
            native_evidence_path: Some("out/native/transcript_events.jsonl".to_string()),
            text_evidence_path: Some("out/text/transcript_events.jsonl".to_string()),
            native_status: Some("failed".to_string()),
            text_status: Some("passed".to_string()),
            native_passed: Some(false),
            text_passed: Some(true),
            native_tool_call_count: Some(0),
            text_tool_call_count: Some(0),
            native_rejected_tool_call_count: Some(0),
            text_rejected_tool_call_count: Some(0),
            verifier_match: Some(false),
            tool_sequence_match: Some(true),
            rejected_tool_call_delta_text_minus_native: Some(0),
            token_delta_text_minus_native: Some(0),
            iteration_delta_text_minus_native: Some(0),
            equivalent: Some(false),
            divergence_reasons: vec!["pass result differs: native=false text=true".to_string()],
            evidence_paths: vec![
                "out/native/transcript_events.jsonl".to_string(),
                "out/text/transcript_events.jsonl".to_string(),
            ],
        }],
        parity_by_pair: vec![ToolModeParityPairSummary {
            provider: "openrouter".to_string(),
            model: "qwen/qwen3-coder".to_string(),
            sample_size: 1,
            agreement_rate: 0.0,
            verifier_divergence_rate: 1.0,
            native: tool_mode_parity::ToolModeParityFormatStats {
                total_runs: 1,
                passed_runs: 0,
                unique_fixtures: 1,
                replicate_count: 1,
                pass_rate: 0.0,
            },
            text: tool_mode_parity::ToolModeParityFormatStats {
                total_runs: 1,
                passed_runs: 1,
                unique_fixtures: 1,
                replicate_count: 1,
                pass_rate: 1.0,
            },
            divergence_counts: tool_mode_parity::ToolModeParityDivergenceCounts {
                native_only_pass: 0,
                text_only_pass: 1,
                both_pass: 0,
                both_fail: 0,
            },
            evidence_paths: vec![
                "out/native/transcript_events.jsonl".to_string(),
                "out/text/transcript_events.jsonl".to_string(),
            ],
        }],
        followups: Vec::new(),
        step_judge_preset: None,
        run_label: String::new(),
        baseline_comparison: None,
    };

    let temp = tempfile::tempdir().expect("tempdir");
    write_json_artifacts(temp.path(), &summary).expect("write artifacts");

    let overlay = crate::commands::tool_mode_parity::read_overlay(
        &temp.path().join(TOOL_MODE_PARITY_OVERLAY_FILENAME),
    )
    .expect("read overlay");
    assert_eq!(overlay.fixture_suite, TOOL_MODE_PARITY_FIXTURE_SUITE);
    assert_eq!(overlay.rows.len(), 1);
    assert_eq!(overlay.rows[0].preferred_tool_format, "text");
    assert!(temp
        .path()
        .join(TOOL_MODE_PARITY_DIRECTORY)
        .join("python-add__openrouter_qwen_qwen3-coder")
        .join("parity.json")
        .exists());
}

#[test]
fn parity_overlay_excludes_skipped_comparisons() {
    let summaries = build_parity_by_pair(&[
        format_comparison_for_test("python-add", "skipped", "skipped"),
        format_comparison_for_test("cli-help-flag", "failed", "passed"),
    ]);

    assert_eq!(summaries.len(), 1);
    let summary = &summaries[0];
    assert_eq!(summary.sample_size, 1);
    assert_eq!(summary.native.total_runs, 1);
    assert_eq!(summary.text.total_runs, 1);
    assert_eq!(summary.text.passed_runs, 1);
    assert_eq!(summary.divergence_counts.text_only_pass, 1);
}

#[test]
fn tool_format_override_warning_line_extracts_first_match() {
    let stderr = "\
debug noise
warning: tool_format override: openrouter:qwen requested native over recommended text (parity: native_unreliable)
warning: something else
";
    assert_eq!(
            tool_format_override_warning_line(stderr),
            Some(
                "warning: tool_format override: openrouter:qwen requested native over recommended text (parity: native_unreliable)"
            )
        );
}

#[test]
fn baseline_comparison_reports_regressions_and_recoveries() {
    // Synthetic baseline summary.json — two fixtures, both passed.
    let tmp = tempfile::tempdir().expect("tempdir");
    let baseline_path = tmp.path().join("baseline_summary.json");
    let baseline = serde_json::json!({
        "schema_version": 2,
        "runs": [
            {"fixture_id": "python-add", "passed": true, "skipped": false},
            {"fixture_id": "cli-help-flag", "passed": true, "skipped": false},
            {"fixture_id": "test-output-first", "passed": false, "skipped": false},
        ],
    });
    std::fs::write(&baseline_path, serde_json::to_string(&baseline).unwrap())
        .expect("write baseline");

    // Cell run: cli-help-flag REGRESSED (was passing), test-output-first RECOVERED.
    let selector = ModelSelector {
        selector: "mock:mock".to_string(),
        provider: "mock".to_string(),
        model: "mock".to_string(),
    };
    let runs = vec![
        RunReport {
            run_id: "r1".to_string(),
            fixture_id: "python-add".to_string(),
            fixture_name: "Python add".to_string(),
            fixture_tool_sequence: "multi-tool".to_string(),
            selector: selector.clone(),
            tool_format: "native".to_string(),
            status: "passed".to_string(),
            passed: true,
            skipped: false,
            skipped_reason: None,
            output_dir: "out/r1".to_string(),
            transcript_events_path: "out/r1/t.jsonl".to_string(),
            workspace_root: None,
            elapsed_ms: 0,
            duration_ms: 0,
            iterations: 0,
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
            pricing_known: false,
            tool_calls: 0,
            rejected_tool_calls: 0,
            tool_sequence: Vec::new(),
            successful_tools: Vec::new(),
            transcript_event_count: 0,
            verification_success: true,
            harn_exit_code: 0,
            error: None,
            stderr_excerpt: None,
            local_cleanup: None,
        },
        RunReport {
            run_id: "r2".to_string(),
            fixture_id: "cli-help-flag".to_string(),
            fixture_name: "CLI help flag".to_string(),
            fixture_tool_sequence: "multi-tool".to_string(),
            selector: selector.clone(),
            tool_format: "native".to_string(),
            status: "failed".to_string(),
            passed: false,
            skipped: false,
            skipped_reason: None,
            output_dir: "out/r2".to_string(),
            transcript_events_path: "out/r2/t.jsonl".to_string(),
            workspace_root: None,
            elapsed_ms: 0,
            duration_ms: 0,
            iterations: 0,
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
            pricing_known: false,
            tool_calls: 0,
            rejected_tool_calls: 0,
            tool_sequence: Vec::new(),
            successful_tools: Vec::new(),
            transcript_event_count: 0,
            verification_success: false,
            harn_exit_code: 1,
            error: None,
            stderr_excerpt: None,
            local_cleanup: None,
        },
        RunReport {
            run_id: "r3".to_string(),
            fixture_id: "test-output-first".to_string(),
            fixture_name: "Test output first".to_string(),
            fixture_tool_sequence: "multi-tool".to_string(),
            selector,
            tool_format: "native".to_string(),
            status: "passed".to_string(),
            passed: true,
            skipped: false,
            skipped_reason: None,
            output_dir: "out/r3".to_string(),
            transcript_events_path: "out/r3/t.jsonl".to_string(),
            workspace_root: None,
            elapsed_ms: 0,
            duration_ms: 0,
            iterations: 0,
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
            pricing_known: false,
            tool_calls: 0,
            rejected_tool_calls: 0,
            tool_sequence: Vec::new(),
            successful_tools: Vec::new(),
            transcript_event_count: 0,
            verification_success: true,
            harn_exit_code: 0,
            error: None,
            stderr_excerpt: None,
            local_cleanup: None,
        },
    ];
    let comparison = load_baseline_comparison(&baseline_path, &runs).expect("compare");
    assert_eq!(comparison.regressions_count, 1);
    assert_eq!(comparison.regressions[0].fixture_id, "cli-help-flag");
    assert_eq!(comparison.recoveries_count, 1);
    assert_eq!(comparison.recoveries[0].fixture_id, "test-output-first");
    assert_eq!(comparison.unchanged_passes, vec!["python-add".to_string()]);
    assert_eq!(
        comparison.net_lift_pp, 0.0,
        "+1 recovery and -1 regression should net to 0pp lift across 3 compared fixtures"
    );
}

#[test]
fn fixture_selection_supports_all_and_specific_ids() {
    let all = resolve_fixtures(&["all".to_string()]).expect("all fixtures resolve");
    assert_eq!(all.len(), FIXTURE_DEFINITIONS.len());

    let selected = resolve_fixtures(&[
        "python-add".to_string(),
        "python-add".to_string(),
        "read-only-audit".to_string(),
    ])
    .expect("specific fixtures resolve");
    assert_eq!(
        selected
            .iter()
            .map(|fixture| fixture.id)
            .collect::<Vec<_>>(),
        vec!["python-add", "read-only-audit"],
    );

    let error = resolve_fixtures(&["missing".to_string()]).expect_err("unknown fixture fails");
    assert!(error.contains("unsupported --fixture `missing`"));
}

#[test]
fn matrix_max_runs_bounds_fixture_model_tool_product() {
    let fixtures = resolve_fixtures(&["all".to_string()]).expect("fixtures");
    let selector = ModelSelector {
        selector: "mock:mock".to_string(),
        provider: "mock".to_string(),
        model: "mock".to_string(),
    };
    let selectors = vec![selector];
    let tool_formats = vec!["native".to_string(), "text".to_string()];
    let matrix = build_matrix(&fixtures, &selectors, &tool_formats, Some(3));
    assert_eq!(matrix.len(), 3);
    assert_eq!(
        matrix
            .iter()
            .map(|(fixture, _selector, tool_format)| (fixture.id, tool_format.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("python-add", "native"),
            ("python-add", "text"),
            ("cli-help-flag", "native"),
        ],
    );

    let empty = build_matrix(&fixtures, &selectors, &tool_formats, Some(0));
    assert!(empty.is_empty());
}
