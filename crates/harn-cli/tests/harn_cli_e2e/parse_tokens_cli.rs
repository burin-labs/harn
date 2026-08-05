//! End-to-end smoke tests for `harn parse --json` and `harn tokens --json`.

use std::process::Command;

fn binary_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_harn"))
}

fn stdout_json(output: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
        panic!("stdout is not JSON: {error}\nstdout:\n{stdout}");
    })
}

#[test]
fn parse_json_emits_program_root_and_tagged_ast_nodes() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let script = temp.path().join("main.harn");
    std::fs::write(&script, "pipeline main(task) {\n  return 1\n}\n").expect("write script");

    let output = Command::new(binary_path())
        .args(["parse", script.to_str().unwrap(), "--json"])
        .output()
        .expect("spawn harn parse --json");
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let parsed = stdout_json(&output);
    assert_eq!(parsed["schemaVersion"], 1);
    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["data"]["kind"], "Program");
    assert_eq!(parsed["data"]["body"][0]["kind"], "Pipeline");
    assert_eq!(parsed["data"]["body"][0]["span"]["start"], 0);
    assert_eq!(parsed["data"]["body"][0]["fields"]["name"], "main");
    assert_eq!(
        parsed["data"]["body"][0]["fields"]["body"][0]["kind"],
        "ReturnStmt"
    );
}

#[test]
fn tokens_json_emits_kinds_lexemes_and_byte_spans() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let script = temp.path().join("main.harn");
    let source = "const x = \"é\"\n// hi\n";
    std::fs::write(&script, source).expect("write script");

    let output = Command::new(binary_path())
        .args(["tokens", script.to_str().unwrap(), "--json"])
        .output()
        .expect("spawn harn tokens --json");
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let parsed = stdout_json(&output);
    assert_eq!(parsed["schemaVersion"], 1);
    assert_eq!(parsed["ok"], true);
    let tokens = parsed["data"].as_array().expect("tokens array");

    let string = tokens
        .iter()
        .find(|token| token["kind"] == "StringLiteral")
        .expect("string literal token");
    let quote = source.find('"').expect("opening quote");
    assert_eq!(string["lexeme"], "\"é\"");
    assert_eq!(string["start"], quote);
    assert_eq!(string["end"], quote + "\"é\"".len());
    assert_eq!(string["line"], 1);
    assert_eq!(string["column"], 11);

    assert!(
        tokens
            .iter()
            .any(|token| token["kind"] == "LineComment" && token["lexeme"] == "// hi"),
        "tokens should include comments: {tokens:?}"
    );
}
