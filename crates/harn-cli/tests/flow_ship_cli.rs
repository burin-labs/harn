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
