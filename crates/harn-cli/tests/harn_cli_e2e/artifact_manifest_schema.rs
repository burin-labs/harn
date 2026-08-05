use serde_json::json;

const ARTIFACT_MANIFEST_SCHEMA: &str =
    include_str!("../../../../spec/schemas/artifact-manifest.v1.schema.json");

#[test]
fn artifact_manifest_schema_validates_document_media_file_refs() {
    let schema: serde_json::Value =
        serde_json::from_str(ARTIFACT_MANIFEST_SCHEMA).expect("artifact manifest schema parses");
    jsonschema::draft202012::meta::validate(&schema).expect("schema is draft 2020-12 valid");
    let validator = jsonschema::draft202012::new(&schema).expect("schema compiles");

    let manifest = json!({
        "schema_version": "harn.artifacts.v1",
        "kind": "artifact_manifest",
        "title": "Code findings",
        "artifact_count": 2,
        "total_size_bytes": 1536,
        "session_id": "ses_123",
        "run_id": "run_456",
        "created_at": "2026-07-05T00:00:00Z",
        "metadata": {
            "contract_package": "@burin/documents",
            "contract_version": "0.1.6"
        },
        "artifacts": [
            {
                "uri": "file:///tmp/report.pdf",
                "name": "report.pdf",
                "mime_type": "application/pdf",
                "path": "/tmp/report.pdf",
                "size_bytes": 1024,
                "sha256": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "description": "Code findings PDF"
            },
            {
                "uri": "artifact://session/render.png",
                "name": "render.png",
                "mime_type": "image/png",
                "relative_path": "render.png",
                "size_bytes": 512,
                "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "metadata": {"page": 1}
            }
        ]
    });

    validator
        .validate(&manifest)
        .expect("representative PDF/PNG manifest validates");
}

#[test]
fn artifact_manifest_schema_rejects_inline_payloads_and_network_refs() {
    let schema: serde_json::Value =
        serde_json::from_str(ARTIFACT_MANIFEST_SCHEMA).expect("artifact manifest schema parses");
    let validator = jsonschema::draft202012::new(&schema).expect("schema compiles");

    let inline_payload = json!({
        "schema_version": "harn.artifacts.v1",
        "kind": "artifact_manifest",
        "artifact_count": 1,
        "artifacts": [
            {
                "uri": "file:///tmp/report.pdf",
                "name": "report.pdf",
                "mime_type": "application/pdf",
                "text": "%PDF-1.7"
            }
        ]
    });
    assert!(
        validator.validate(&inline_payload).is_err(),
        "file artifact specs must reference payloads instead of embedding them"
    );

    let network_ref = json!({
        "schema_version": "harn.artifacts.v1",
        "kind": "artifact_manifest",
        "artifact_count": 1,
        "artifacts": [
            {
                "uri": "https://example.com/report.pdf",
                "name": "report.pdf",
                "mime_type": "application/pdf"
            }
        ]
    });
    assert!(
        validator.validate(&network_ref).is_err(),
        "artifact manifest refs must not silently smuggle network fetches"
    );
}

#[test]
fn artifact_manifest_schema_matches_artifact_emit_unknown_key_policy() {
    let schema: serde_json::Value =
        serde_json::from_str(ARTIFACT_MANIFEST_SCHEMA).expect("artifact manifest schema parses");
    let validator = jsonschema::draft202012::new(&schema).expect("schema compiles");

    let unknown_manifest_key = json!({
        "schema_version": "harn.artifacts.v1",
        "kind": "artifact_manifest",
        "artifact_count": 0,
        "artifacts": [],
        "schema_url": "https://harnlang.com/schemas/artifact-manifest.v1.schema.json"
    });
    assert!(
        validator.validate(&unknown_manifest_key).is_err(),
        "artifact manifests must keep extension fields under metadata"
    );

    let unknown_file_key = json!({
        "schema_version": "harn.artifacts.v1",
        "kind": "artifact_manifest",
        "artifact_count": 1,
        "artifacts": [
            {
                "uri": "file:///tmp/report.pdf",
                "name": "report.pdf",
                "mime_type": "application/pdf",
                "schema_url": "https://harnlang.com/schemas/artifact-manifest.v1.schema.json"
            }
        ]
    });
    assert!(
        validator.validate(&unknown_file_key).is_err(),
        "file artifact specs must keep extension fields under metadata"
    );
}
