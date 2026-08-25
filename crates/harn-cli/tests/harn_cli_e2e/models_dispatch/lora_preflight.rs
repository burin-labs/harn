use std::fs;

use super::lora_fixtures::write_lora_corpus_fixture;
use super::support::{parse_json, run};

#[test]
fn models_lora_preflight_honors_configured_record_floor() {
    let corpus = write_lora_corpus_fixture();
    let config = corpus.path().join("config.yaml");
    fs::write(
        &config,
        "max_seq_length: 4096\nmin_fit_ratio: 1.0\nmin_records: 500\n",
    )
    .expect("write config");
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
            "--config",
            config.to_str().expect("utf8 config path"),
            "--check",
            "--json",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 1, "harn stdout={}", harn.stdout);
    let harn_value = parse_json(&harn.stdout, "harn");
    let details = &harn_value["error"]["details"];
    assert_eq!(details["config"]["min_records"], 500);
    assert_eq!(details["thresholds"]["min_records"], 500);
    assert!(details["errors"]
        .as_array()
        .expect("errors")
        .iter()
        .any(|error| error == "records 5 below required floor 500"));
}

#[test]
fn models_lora_preflight_check_requires_record_floor() {
    let corpus = write_lora_corpus_fixture();
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
            "--check",
            "--json",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 1, "harn stdout={}", harn.stdout);
    let harn_value = parse_json(&harn.stdout, "harn");
    let details = &harn_value["error"]["details"];
    assert!(details["thresholds"]["min_records"].is_null());
    assert!(details["errors"]
        .as_array()
        .expect("errors")
        .iter()
        .any(|error| error
            .as_str()
            .is_some_and(|text| text.contains("minimum record floor is required under --check"))));
}
