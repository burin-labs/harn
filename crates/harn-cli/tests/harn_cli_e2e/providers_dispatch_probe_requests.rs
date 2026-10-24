use serde_json::{json, Value};

use crate::test_util::cli::parse_json;
use crate::test_util::process::run_harn_e2e as run;

#[test]
fn provider_tool_probe_dry_run_request_large_case_is_offline_and_deterministic() {
    for (model, dialect, name_path, schema_path, prompt_path, absent_field, tool_choice) in [
        (
            "gpt-5.1",
            "openai_compat",
            "/tools/0/function/name",
            "/tools/0/function/parameters/properties/value/type",
            "/messages/0/content",
            "input",
            json!({"type": "function", "function": {"name": "echo_marker"}}),
        ),
        (
            "gpt-5.4-mini",
            "openai_responses",
            "/tools/0/name",
            "/tools/0/parameters/properties/value/type",
            "/input/0/content",
            "messages",
            json!({"type": "function", "name": "echo_marker"}),
        ),
    ] {
        let argv = [
            "provider",
            "tool-probe",
            "openai",
            "--model",
            model,
            "--mode",
            "non-streaming",
            "--case",
            "large_string_argument",
            "--marker",
            "case-seed",
            "--dry-run-request",
        ];
        let harn = run(&argv, &[]);
        let repeat = run(&argv, &[]);
        assert_eq!(harn.exit_code, 0, "{model}: {}", harn.stderr);
        assert_eq!(repeat.exit_code, 0, "{model}: {}", repeat.stderr);
        let report = parse_json(&harn.stdout, "harn");
        assert_eq!(
            parse_json(&repeat.stdout, "repeat"),
            report,
            "{model}: dry-run JSON must be deterministic",
        );
        assert_eq!(
            report["schema_version"],
            harn_vm::llm::tool_conformance::TOOL_CONFORMANCE_REQUEST_SCHEMA_VERSION,
        );
        assert_eq!(report["probe_case"], "large_string_argument");
        assert!(report["expected_value"]
            .as_str()
            .expect("large expected value")
            .contains("heredoc=<<EOF"));
        assert_eq!(report["requests"].as_array().map(Vec::len), Some(1));
        assert_eq!(report["requests"][0]["validation"]["dialect"], dialect);
        assert_eq!(report["requests"][0]["validation"]["status"], "pass");
        let request = &report["requests"][0]["request_body"];
        assert_eq!(request["tools"][0]["type"], "function");
        assert_eq!(
            request.pointer(name_path).and_then(Value::as_str),
            Some("echo_marker"),
        );
        assert_eq!(
            request.pointer(schema_path).and_then(Value::as_str),
            Some("string"),
        );
        let prompt = request
            .pointer(prompt_path)
            .and_then(Value::as_str)
            .expect("nonempty native-tool prompt");
        assert!(prompt.contains("case-seed") && prompt.contains("heredoc=<<EOF"));
        assert!(request.get(absent_field).is_none());
        assert_eq!(request["tool_choice"], tool_choice);
        assert!(request.get("stream").is_none());
    }
}
