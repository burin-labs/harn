//! End-to-end for `harn eval calibrate`.
//!
//! Spawns the binary so the clap parser, the embedded-script dispatch, and the
//! rendering all run on the real path. The three cases that matter are the
//! three exit codes: a report, a typed refusal, and inputs that do not join.

use crate::test_util::process::{run_harn_e2e, HarnCliOutput};

fn write(dir: &std::path::Path, name: &str, lines: &[String]) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).expect("write jsonl");
    path
}

/// Twenty rows at 0.95 with one wrong, twenty at 0.55 with nine wrong.
fn calibrated_corpus() -> (Vec<String>, Vec<String>) {
    let mut corpus = Vec::new();
    let mut answers = Vec::new();
    for index in 0..40 {
        let high = index < 20;
        let confidence = if high { 0.95 } else { 0.55 };
        let correct = if high { index != 0 } else { index < 31 };
        corpus.push(format!(
            r#"{{"id": "r{index}", "question_id": "tool-safety", "expected": "true"}}"#
        ));
        answers.push(format!(
            r#"{{"id": "r{index}", "question_id": "tool-safety", "predicted": "{}", "confidence": {confidence}, "abstained": false, "latency_ms": 120.0}}"#,
            if correct { "true" } else { "false" }
        ));
    }
    (corpus, answers)
}

fn run(args: &[&str]) -> HarnCliOutput {
    run_harn_e2e(args, &[])
}

#[test]
fn calibrate_reports_in_plain_language_and_json() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (corpus_rows, answer_rows) = calibrated_corpus();
    let corpus = write(tmp.path(), "corpus.jsonl", &corpus_rows);
    let answers = write(tmp.path(), "answers.jsonl", &answer_rows);

    let text = run(&[
        "eval",
        "calibrate",
        "--corpus",
        &corpus.to_string_lossy(),
        "--answers",
        &answers.to_string_lossy(),
        "--thresholds",
        "0.9",
        "--served-model-id",
        "example/model@1",
    ]);
    assert_eq!(text.exit_code, 0);
    let rendered = text.stdout;
    assert!(
        rendered.contains("tool-safety question"),
        "rendering names the question: {rendered}"
    );
    assert!(
        rendered.contains("percent") && rendered.contains("threshold"),
        "rendering is plain language with a threshold line: {rendered}"
    );

    let json = run(&[
        "eval",
        "calibrate",
        "--corpus",
        &corpus.to_string_lossy(),
        "--answers",
        &answers.to_string_lossy(),
        "--thresholds",
        "0.9",
        "--served-model-id",
        "example/model@1",
        "--json",
    ]);
    assert_eq!(json.exit_code, 0);
    let stdout = json.stdout;
    let json_text = stdout
        .split_once('{')
        .map(|(_, rest)| format!("{{{rest}"))
        .unwrap_or_else(|| panic!("no JSON: {stdout}"));
    let report: serde_json::Value = serde_json::from_str(json_text.trim()).expect("report json");
    assert_eq!(report["kind"], "report");
    assert_eq!(report["contract"], "harn.calibration_report.v1");
    assert_eq!(report["served_model_id"], "example/model@1");
    assert_eq!(report["rows"], 40);
    let group = &report["groups"][0];
    assert_eq!(group["question_id"], "tool-safety");
    assert_eq!(group["scored_rows"], 40);
    // The 0.95 bin is right 19 of 20 and the 0.55 bin 11 of 20, so the curve
    // sits on the diagonal and the threshold row carries its own denominator.
    assert_eq!(group["bins"][9]["rows"], 20);
    assert_eq!(group["bins"][9]["accuracy"], 0.95);
    assert_eq!(group["thresholds"][0]["accepted"], 20);
    assert_eq!(group["thresholds"][0]["false_accept"], 1);
    assert!(
        group["latency_ms"]["rows"].as_i64() == Some(40),
        "latency distribution carries its row count: {group}"
    );
}

#[test]
fn calibrate_exits_one_on_a_typed_refusal() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let corpus = write(
        tmp.path(),
        "corpus.jsonl",
        &[
            r#"{"id": "a", "question_id": "q", "expected": "true"}"#.to_string(),
            r#"{"id": "b", "question_id": "q", "expected": "true"}"#.to_string(),
        ],
    );
    // A confidence above 1.0 is not a probability. The report refuses rather
    // than binning it as if it were.
    let answers = write(
        tmp.path(),
        "answers.jsonl",
        &[
            r#"{"id": "a", "question_id": "q", "predicted": "true", "confidence": 0.9}"#
                .to_string(),
            r#"{"id": "b", "question_id": "q", "predicted": "true", "confidence": 4.0}"#
                .to_string(),
        ],
    );
    let out = run(&[
        "eval",
        "calibrate",
        "--corpus",
        &corpus.to_string_lossy(),
        "--answers",
        &answers.to_string_lossy(),
    ]);
    assert_eq!(out.exit_code, 1);
    let stderr = out.stderr;
    assert!(
        stderr.contains("invalid_confidence"),
        "refusal names its reason: {stderr}"
    );
}

#[test]
fn calibrate_refuses_inputs_that_do_not_join() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let corpus = write(
        tmp.path(),
        "corpus.jsonl",
        &[
            r#"{"id": "a", "question_id": "q", "expected": "true"}"#.to_string(),
            r#"{"id": "b", "question_id": "q", "expected": "true"}"#.to_string(),
        ],
    );
    // Only one of the two corpus rows was answered. A silent inner join would
    // report confidently about half the corpus.
    let answers = write(
        tmp.path(),
        "answers.jsonl",
        &[
            r#"{"id": "a", "question_id": "q", "predicted": "true", "confidence": 0.9}"#
                .to_string(),
        ],
    );
    let out = run(&[
        "eval",
        "calibrate",
        "--corpus",
        &corpus.to_string_lossy(),
        "--answers",
        &answers.to_string_lossy(),
    ]);
    assert_eq!(out.exit_code, 2);
    let stderr = out.stderr;
    assert!(
        stderr.contains("do not line up") && stderr.contains("corpus rows with no answer"),
        "the gap is named, not swallowed: {stderr}"
    );
}
