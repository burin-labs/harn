//! A receipt emitted by the actual CLI is the positive binding control.
use serde_json::{json, Value};
use std::path::Path;

fn run(root: &Path, args: &[&str]) -> (i32, Value) {
    let output = crate::test_util::process::harn_e2e_command()
        .env_clear()
        .env("HOME", root)
        .env("HARN_LLM_CALLS_DISABLED", "1")
        .current_dir(root)
        .args(args)
        .output()
        .expect("run canonical CLI without credentials");
    let value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "JSON: {error}; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.code().expect("exit code"), value)
}

fn write(root: &Path, name: &str, value: &Value) {
    std::fs::write(root.join(name), serde_json::to_vec(value).unwrap()).unwrap();
}

fn verify(root: &Path) -> (i32, Value) {
    run(
        root,
        &[
            "llm",
            "evaluate",
            "--request",
            "request.json",
            "--verify-receipt",
            "receipt.json",
            "--json",
        ],
    )
}

#[test]
fn emitted_receipt_binds_request_offline_and_refuses_tampering() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let request = json!({
        "site_id":"receipt.binding.control",
        "state":{"a":1,"b":2},
        "questions":{"action":{"kind":"choice","instructions":"What happened?",
            "criteria":{"read":"Only read a file","write":"Changed a file"}}},
        "policy":{"backend":"native_decision","provider":"openrouter",
            "model":"openrouter/typesafe/jev-1.13","threshold":0.5,
            "evaluation_cost_limit":0.0,"run_cost_limit":0.01}
    });
    write(root, "request.json", &request);
    let (status, emitted) = run(
        root,
        &["llm", "evaluate", "--request", "request.json", "--json"],
    );
    assert_eq!(status, 0);
    assert_eq!(emitted["outcome"]["kind"], "budget_cut");
    assert_eq!(emitted["receipt"]["physical_attempts"], 0);
    let receipt = emitted["receipt"].clone();
    write(root, "receipt.json", &receipt);
    let (status, result) = verify(root);
    assert_eq!(status, 0);
    assert_eq!(result["verified"], true);
    assert_eq!(result["refusals"], json!([]));
    assert_eq!(result["stable_request_id"], receipt["evaluation_id"]);

    // Different JSON key order and whitespace preserve the same semantic input.
    let reordered = format!(
        "{{\n\"state\":{{\"b\":2,\"a\":1}},\"policy\":{},\"questions\":{},\"site_id\":{}\n}}",
        request["policy"], request["questions"], request["site_id"]
    );
    std::fs::write(root.join("request.json"), reordered).unwrap();
    assert_eq!(verify(root).0, 0);

    for (pointer, replacement, code) in [
        ("/state/a", json!(9), "input_mismatch"),
        (
            "/questions/action/criteria/read",
            json!("Changed rubric"),
            "questions_mismatch",
        ),
        ("/policy/threshold", json!(0.9), "policy_mismatch"),
        ("/site_id", json!("another.site"), "site_mismatch"),
    ] {
        let mut changed = request.clone();
        *changed.pointer_mut(pointer).unwrap() = replacement;
        write(root, "request.json", &changed);
        let (status, result) = verify(root);
        assert_eq!(status, 1, "{pointer}: {result}");
        assert_eq!(result["verified"], false);
        assert!(result["refusals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["code"] == code));
    }
    write(root, "request.json", &request);
    for (pointer, replacement, code) in [
        (
            "/identity/contract_version",
            json!("future"),
            "unsupported_contract",
        ),
        (
            "/identity/evaluator_instruction_version",
            json!("future"),
            "unsupported_contract",
        ),
        (
            "/identity/protocol",
            json!("unknown"),
            "unsupported_contract",
        ),
        ("/evaluation_id", json!("tampered"), "identity_mismatch"),
    ] {
        let mut changed = receipt.clone();
        *changed.pointer_mut(pointer).unwrap() = replacement;
        write(root, "receipt.json", &changed);
        let (status, result) = verify(root);
        assert_eq!(status, 1, "{pointer}: {result}");
        assert!(result["refusals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["code"] == code));
    }
    let mut legacy = receipt.clone();
    legacy["identity"]
        .as_object_mut()
        .unwrap()
        .remove("contract_version");
    write(root, "receipt.json", &legacy);
    let (status, result) = verify(root);
    assert_eq!(status, 1);
    assert_eq!(result["refusals"][0]["code"], "unsupported_contract");

    // Occurrence provenance is not part of stable request identity.
    let mut replayed = receipt;
    replayed["source"] = json!("tape");
    write(root, "receipt.json", &replayed);
    assert_eq!(verify(root).0, 0);
}
