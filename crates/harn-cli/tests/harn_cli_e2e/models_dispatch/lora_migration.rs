use super::lora_fixtures::write_lora_behavior_migration_fixture;
use super::support::{parse_json, run, success_data};

#[test]
fn models_lora_preflight_legacy_corpus_is_strict_by_default() {
    let corpus = write_lora_behavior_migration_fixture(None);
    let harn = run(
        &[
            "models",
            "lora",
            "preflight",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--corpus",
            corpus.path().to_str().expect("utf8 corpus path"),
            "--max-seq-length",
            "4096",
            "--min-tool-call-share",
            "1",
            "--check",
            "--json",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 1, "harn stdout={}", harn.stdout);
    let envelope = parse_json(&harn.stdout, "harn");
    let report = &envelope["error"]["details"];
    let strata = &report["stats"]["behavior_strata"];
    assert_eq!(strata["policy"], "strict");
    assert_eq!(strata["status"], "incomplete");
    assert_eq!(
        strata["missing_required"]
            .as_array()
            .expect("missing classes")
            .len(),
        5
    );
    assert_eq!(
        strata["unclassified_record_ids"],
        serde_json::json!(["legacy-read"])
    );
}

#[test]
fn models_lora_preflight_legacy_policy_reports_unclassified_without_coverage() {
    let corpus = write_lora_behavior_migration_fixture(None);
    let args = [
        "models",
        "lora",
        "preflight",
        "--base",
        "local-gemma4-e4b",
        "--provider",
        "vllm",
        "--corpus",
        corpus.path().to_str().expect("utf8 corpus path"),
        "--max-seq-length",
        "4096",
        "--min-tool-call-share",
        "1",
        "--behavior-strata-policy",
        "legacy-unclassified",
        "--check",
    ];
    let human = run(&args, &[]);
    assert_eq!(human.exit_code, 0, "harn stderr={}", human.stderr);
    for fragment in [
        "behavior strata policy: legacy_unclassified status=legacy_unclassified",
        "missing behavior strata:",
        "unclassified record ids:",
        "- legacy-read",
        "WARNING: legacy corpus has no declared behavior-strata metadata",
        "result: MIGRATION REVIEW",
    ] {
        assert!(
            human.stdout.contains(fragment),
            "harn stdout missing {fragment}: {}",
            human.stdout
        );
    }

    let mut json_args = args.to_vec();
    json_args.push("--json");
    let json = run(&json_args, &[]);
    assert_eq!(json.exit_code, 0, "harn stderr={}", json.stderr);
    let envelope = parse_json(&json.stdout, "harn");
    let report = success_data(&envelope);
    let strata = &report["stats"]["behavior_strata"];
    assert_eq!(strata["policy"], "legacy_unclassified");
    assert_eq!(strata["status"], "legacy_unclassified");
    assert_eq!(strata["source"], serde_json::json!({}));
    assert_eq!(strata["emitted"], serde_json::json!({}));
    assert_eq!(
        strata["missing_required"]
            .as_array()
            .expect("missing classes")
            .len(),
        5
    );
    assert_eq!(
        strata["unclassified_record_ids"],
        serde_json::json!(["legacy-read"])
    );
    assert!(report["warnings"]
        .as_array()
        .expect("warnings")
        .iter()
        .any(|warning| warning
            .as_str()
            .is_some_and(|text| text.starts_with("WARNING: legacy corpus"))));
}

#[test]
fn models_lora_preflight_legacy_policy_cannot_hide_partial_coverage() {
    let corpus = write_lora_behavior_migration_fixture(Some("valid_tool_call"));
    let harn = run(
        &[
            "models",
            "lora",
            "preflight",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--corpus",
            corpus.path().to_str().expect("utf8 corpus path"),
            "--max-seq-length",
            "4096",
            "--min-tool-call-share",
            "1",
            "--behavior-strata-policy",
            "legacy-unclassified",
            "--check",
            "--json",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 1, "harn stdout={}", harn.stdout);
    let envelope = parse_json(&harn.stdout, "harn");
    let report = &envelope["error"]["details"];
    let strata = &report["stats"]["behavior_strata"];
    assert_eq!(strata["policy"], "legacy_unclassified");
    assert_eq!(strata["status"], "incomplete");
    assert_eq!(strata["source"]["valid_tool_call"], 1);
    assert_eq!(
        strata["missing_required"]
            .as_array()
            .expect("missing classes")
            .len(),
        4
    );
    assert!(report["errors"]
        .as_array()
        .expect("errors")
        .iter()
        .any(|error| error
            .as_str()
            .is_some_and(|text| text.contains("missing required behavior strata"))));
}

#[test]
fn models_lora_export_remains_strict_for_legacy_corpus() {
    let corpus = write_lora_behavior_migration_fixture(None);
    let harn = run(
        &[
            "models",
            "lora",
            "export",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--tool-format",
            "native",
            "--corpus",
            corpus.path().to_str().expect("utf8 corpus path"),
            "--check",
            "--json",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 1, "harn stdout={}", harn.stdout);
    let envelope = parse_json(&harn.stdout, "harn");
    let report = &envelope["error"]["details"];
    let strata = &report["stats"]["behavior_strata"];
    assert_eq!(strata["policy"], "strict");
    assert_eq!(strata["status"], "incomplete");
    assert_eq!(
        strata["missing_required"]
            .as_array()
            .expect("missing classes")
            .len(),
        5
    );
    assert!(report["errors"]
        .as_array()
        .expect("errors")
        .iter()
        .any(|error| error
            .as_str()
            .is_some_and(|text| text.contains("missing required behavior strata"))));
}
