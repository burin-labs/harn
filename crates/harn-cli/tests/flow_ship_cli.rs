use std::fs;
use std::process::Command;

use ed25519_dalek::SigningKey;
use harn_vm::flow::{Atom, AtomId, Provenance, SqliteFlowStore, TextOp, VcsBackend};
use tempfile::TempDir;
use time::OffsetDateTime;

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn atom(index: u64, parents: Vec<AtomId>) -> Atom {
    let principal = key(1);
    let persona = key(2);
    Atom::sign(
        vec![TextOp::Insert {
            offset: index,
            content: format!("atom-{index}\n"),
        }],
        parents,
        Provenance {
            principal: "user:alice".to_string(),
            persona: "ship_captain".to_string(),
            agent_run_id: "run-flow-demo".to_string(),
            tool_call_id: Some(format!("tool-{index}")),
            trace_id: "trace-flow-demo".to_string(),
            transcript_ref: "transcript-flow-demo".to_string(),
            timestamp: OffsetDateTime::from_unix_timestamp(1_777_200_000 + index as i64).unwrap(),
        },
        None,
        &principal,
        &persona,
    )
    .unwrap()
}

fn demo_repo() -> TempDir {
    let temp = TempDir::new().unwrap();
    fs::create_dir_all(temp.path().join(".harn")).unwrap();
    fs::write(
        temp.path().join("invariants.harn"),
        r#"
@invariant
@deterministic
@archivist(
  evidence: ["https://github.com/burin-labs/harn/issues/585"],
  confidence: 0.9,
  source_date: "2026-04-26",
  coverage_examples: ["flow-demo"]
)
fn phase_zero_demo(slice) {
  return flow_invariant_allow()
}
"#,
    )
    .unwrap();
    temp
}

#[test]
fn flow_ship_watch_injects_atoms_and_opens_mock_pr() {
    let repo = demo_repo();
    let store_path = repo.path().join(".harn/flow.sqlite");
    let store = SqliteFlowStore::open(&store_path, "flow-demo").unwrap();
    let mut atoms = Vec::new();
    for index in 0..10 {
        let parents = atoms
            .last()
            .map(|atom: &Atom| vec![atom.id])
            .unwrap_or_default();
        atoms.push(atom(index, parents));
    }
    for atom in &atoms {
        store.emit_atom(atom).unwrap();
    }
    drop(store);

    let mock_pr_path = repo.path().join(".harn/flow/mock-pr.json");
    let output = Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(repo.path())
        .args([
            "flow",
            "ship",
            "watch",
            "--store",
            store_path.to_str().unwrap(),
            "--mock-pr-out",
            mock_pr_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout_payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let file_payload: serde_json::Value =
        serde_json::from_slice(&fs::read(&mock_pr_path).unwrap()).unwrap();
    assert_eq!(stdout_payload, file_payload);
    assert_eq!(file_payload["status"], "mock_pr_opened");
    assert_eq!(file_payload["persona"], "ship_captain");
    assert_eq!(file_payload["autonomy"], "propose_with_approval");
    assert_eq!(file_payload["slice"]["atom_count"], 10);
    assert_eq!(file_payload["slice"]["atoms"].as_array().unwrap().len(), 10);
    assert_eq!(file_payload["intents"].as_array().unwrap().len(), 1);
    assert_eq!(file_payload["mock_pr"]["state"], "open");
    assert_eq!(file_payload["mock_pr"]["requires_approval"], true);
    assert_eq!(
        file_payload["predicate_validation"]["predicates"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(file_payload["predicate_validation"]["status"], "ok");
    assert_eq!(file_payload["mock_pr"]["validation_status"], "ok");
    let ceiling = &file_payload["predicate_validation"]["ceiling"];
    assert_eq!(ceiling["status"], "within");
    assert_eq!(ceiling["count"], 1);
    assert_eq!(ceiling["require_approval_threshold"], 256);
    assert_eq!(ceiling["block_threshold"], 1024);
    assert_eq!(
        file_payload["eval_packs"].as_array().unwrap(),
        &[
            serde_json::json!("slice_quality"),
            serde_json::json!("false_ship_rate"),
            serde_json::json!("coverage_fidelity"),
            serde_json::json!("latency_pr_to_merge"),
        ]
    );
    assert!(file_payload["ship_receipt"]["ref_name"]
        .as_str()
        .unwrap()
        .starts_with("sqlite://slices/"));
}

fn predicate_block(name: &str) -> String {
    format!(
        r#"
@invariant
@deterministic
@archivist(
  evidence: ["https://github.com/burin-labs/harn/issues/733"],
  confidence: 0.9,
  source_date: "2026-04-26",
  coverage_examples: ["explosion-fixture"]
)
fn {name}(slice) {{
  return flow_invariant_allow()
}}
"#,
    )
}

fn write_invariants_with_many_predicates(dir: &std::path::Path, count: usize) {
    fs::create_dir_all(dir).unwrap();
    let mut body = String::new();
    for index in 0..count {
        body.push_str(&predicate_block(&format!("pred_{index:04}")));
    }
    fs::write(dir.join("invariants.harn"), body).unwrap();
}

#[test]
fn flow_ship_watch_surfaces_bootstrap_policy_when_present() {
    let repo = demo_repo();
    fs::write(
        repo.path().join("meta-invariants.harn"),
        r#"
@bootstrap_maintainers(approvers: ["role:flow-platform", "user:alice"])
fn _bootstrap_marker() {
  return nil
}
"#,
    )
    .unwrap();
    let store_path = repo.path().join(".harn/flow.sqlite");
    let store = SqliteFlowStore::open(&store_path, "flow-bootstrap").unwrap();
    let mut atoms = Vec::new();
    for index in 0..3 {
        let parents = atoms
            .last()
            .map(|atom: &Atom| vec![atom.id])
            .unwrap_or_default();
        atoms.push(atom(index, parents));
    }
    for atom in &atoms {
        store.emit_atom(atom).unwrap();
    }
    drop(store);

    let mock_pr_path = repo.path().join(".harn/flow/mock-pr.json");
    let output = Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(repo.path())
        .args([
            "flow",
            "ship",
            "watch",
            "--store",
            store_path.to_str().unwrap(),
            "--mock-pr-out",
            mock_pr_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let bootstrap = &payload["predicate_validation"]["bootstrap_policy"];
    assert_eq!(bootstrap["status"], "present");
    let hash = bootstrap["hash"].as_str().unwrap();
    assert!(hash.starts_with("sha256:"), "{hash}");
    let maintainers = bootstrap["maintainers"].as_array().unwrap();
    assert_eq!(maintainers.len(), 2);
    assert_eq!(maintainers[0]["kind"], "role");
    assert_eq!(maintainers[0]["id"], "flow-platform");
    assert_eq!(maintainers[1]["kind"], "principal");
    assert_eq!(maintainers[1]["id"], "user:alice");
}

#[test]
fn flow_ship_watch_marks_bootstrap_policy_absent_when_missing() {
    let repo = demo_repo();
    let store_path = repo.path().join(".harn/flow.sqlite");
    let store = SqliteFlowStore::open(&store_path, "flow-bootstrap-absent").unwrap();
    store.emit_atom(&atom(0, Vec::new())).unwrap();
    drop(store);

    let output = Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(repo.path())
        .args([
            "flow",
            "ship",
            "watch",
            "--store",
            store_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());

    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        payload["predicate_validation"]["bootstrap_policy"]["status"],
        "absent"
    );
}

#[test]
fn flow_ship_watch_blocks_when_predicate_union_explodes() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join(".harn")).unwrap();
    // Far above the 1024 hard ceiling — the leaf alone contributes 1100
    // sibling-specific predicates so de-dup cannot collapse them.
    write_invariants_with_many_predicates(repo, 1100);

    let store_path = repo.join(".harn/flow.sqlite");
    let store = SqliteFlowStore::open(&store_path, "flow-explosion").unwrap();
    let mut atoms = Vec::new();
    for index in 0..3 {
        let parents = atoms
            .last()
            .map(|atom: &Atom| vec![atom.id])
            .unwrap_or_default();
        atoms.push(atom(index, parents));
    }
    for atom in &atoms {
        store.emit_atom(atom).unwrap();
    }
    drop(store);

    let mock_pr_path = repo.join(".harn/flow/mock-pr.json");
    let output = Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(repo)
        .args([
            "flow",
            "ship",
            "watch",
            "--store",
            store_path.to_str().unwrap(),
            "--mock-pr-out",
            mock_pr_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["predicate_validation"]["status"], "blocked");
    assert_eq!(payload["mock_pr"]["validation_status"], "blocked");
    let ceiling = &payload["predicate_validation"]["ceiling"];
    assert_eq!(ceiling["status"], "blocked");
    assert_eq!(ceiling["count"], 1100);
    assert_eq!(ceiling["threshold"], 1024);
    let message = ceiling["message"].as_str().unwrap();
    assert!(
        message.contains("hard ceiling") && message.contains("1100"),
        "unexpected ceiling message: {message}"
    );
    let contributors = ceiling["top_contributors"].as_array().unwrap();
    assert!(!contributors.is_empty());
}
