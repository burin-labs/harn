use std::fs;

use super::super::transcript::discover_template_renders;

#[test]
fn joins_snapshots_and_preserves_legacy_inline_events() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("run.json"), "{}").unwrap();
    let llm_dir = temp.path().join("run-llm");
    fs::create_dir_all(&llm_dir).unwrap();
    let snapshot = serde_json::json!({
        "provider": "local",
        "model": "qwen",
        "family": "qwen",
        "capabilities": {
            "native_tools": true,
            "future_sensitive_field": "retained-before-policy-tightening",
        },
    });
    let snapshot_id = harn_vm::llm::capability_snapshot_id(&snapshot);
    let rows = [
        serde_json::json!({
            "type": "llm.capability_snapshot",
            "schema": "harn.llm.capability_snapshot.v1",
            "snapshot_id": snapshot_id,
            "llm": snapshot,
        }),
        serde_json::json!({
            "type": "template.render",
            "template_uri": "prompt.harn.prompt",
            "template_revision_hash": "rev",
            "rendered_bytes": 42,
            "llm": {
                "provider": "tampered-inline-provider",
                "model": "tampered-inline-model",
                "family": "tampered-inline-family",
                "capability_snapshot_ref": snapshot_id,
            },
            "branches": [],
        }),
        serde_json::json!({
            "type": "template.render",
            "template_uri": "prompt.harn.prompt",
            "template_revision_hash": "rev",
            "rendered_bytes": 42,
            "llm": {
                "provider": "local",
                "model": "qwen",
                "family": "qwen",
                "capabilities": {"native_tools": true},
            },
            "branches": [],
        }),
    ];
    let transcript = rows
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(
        llm_dir.join("llm_transcript.jsonl"),
        format!("{transcript}\n"),
    )
    .unwrap();

    // Snapshot IDs name the exact bytes retained by the writer. A later
    // tightening of display redaction must not make a valid historical
    // definition fail integrity verification; redact only after verifying it.
    let _policy = harn_vm::redact::PolicyGuard::new(
        harn_vm::redact::RedactionPolicy::default().with_extra_field("future_sensitive_field"),
    );
    let renders = discover_template_renders(temp.path(), "run.json").unwrap();
    assert_eq!(renders.len(), 2);
    for render in &renders {
        assert_eq!(render.provider, "local");
        assert_eq!(render.model, "qwen");
        assert_eq!(render.family, "qwen");
        assert_eq!(
            render.capabilities.get("native_tools"),
            Some(&serde_json::Value::Bool(true))
        );
    }
    assert_eq!(
        renders[0].capabilities.get("future_sensitive_field"),
        Some(&serde_json::Value::String(
            harn_vm::redact::REDACTED_PLACEHOLDER.to_string()
        ))
    );
    assert!(
        !renders[1]
            .capabilities
            .contains_key("future_sensitive_field"),
        "legacy inline records must not invent fields that were never present"
    );
}

#[test]
fn rejects_a_snapshot_body_that_mismatches_its_id() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("run.json"), "{}").unwrap();
    let llm_dir = temp.path().join("run-llm");
    fs::create_dir_all(&llm_dir).unwrap();
    let trusted = serde_json::json!({"capabilities": {"native_tools": true}});
    let trusted_id = harn_vm::llm::capability_snapshot_id(&trusted);
    let transcript = format!(
        "{}\n{}\n",
        serde_json::json!({
            "type": "llm.capability_snapshot",
            "schema": "harn.llm.capability_snapshot.v1",
            "snapshot_id": trusted_id,
            "llm": {"capabilities": {"native_tools": false}},
        }),
        serde_json::json!({
            "type": "template.render",
            "llm": {
                "provider": "forged-inline-provider",
                "model": "forged-inline-model",
                "family": "forged-inline-family",
                "capabilities": {"native_tools": false},
                "capability_snapshot_ref": trusted_id,
            },
        }),
    );
    fs::write(llm_dir.join("llm_transcript.jsonl"), transcript).unwrap();

    let renders = discover_template_renders(temp.path(), "run.json").unwrap();
    assert_eq!(renders.len(), 1);
    assert!(
        renders[0].capabilities.is_empty(),
        "an invalid content-addressed definition must not fall back to inline capabilities"
    );
    assert_eq!(renders[0].provider, "");
    assert_eq!(renders[0].model, "");
    assert_eq!(renders[0].family, "");
}
