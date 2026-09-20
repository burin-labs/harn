use super::super::session_update_payloads::{SessionUpdatePayloads, SOURCE};
use super::*;
use std::collections::BTreeSet;

fn adapter_notifications() -> Vec<serde_json::Value> {
    // The adapter's harn_extension_session_update_fixtures_are_pinned test
    // compares this fixture against real AgentEvent emission.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../harn-serve/tests/fixtures/acp/session_update_extensions.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("adapter fixture"))
        .expect("adapter notifications")
}

#[test]
fn generated_session_updates_round_trip_the_adapter_fixture() {
    let notifications = adapter_notifications();
    let kinds: BTreeSet<_> = notifications
        .iter()
        .map(|notification| {
            notification["params"]["update"]["sessionUpdate"]
                .as_str()
                .expect("kind")
        })
        .collect();
    assert_eq!(
        kinds.len(),
        17,
        "the producer fixture must exercise all 17 kinds"
    );
    for notification in notifications {
        let update = notification["params"]["update"].clone();
        let decoded =
            serde_json::from_value::<generated_rust_binding::ACPTypedSessionUpdate>(update.clone())
                .unwrap_or_else(|error| panic!("{}: {error}", update["sessionUpdate"]));
        assert_eq!(
            serde_json::to_value(decoded).unwrap(),
            update,
            "typed decoding must preserve every emitted field"
        );
    }
}

#[test]
fn session_update_schema_accepts_canonical_metadata_and_refuses_missing_identity() {
    let schema: serde_json::Value =
        serde_json::from_str(&protocol_source().read_text(SOURCE).unwrap()).unwrap();
    jsonschema::meta::validate(&schema).expect("valid owning schema");
    let validator = jsonschema::draft202012::new(&schema).unwrap();
    for mut notification in adapter_notifications() {
        let kind = notification["params"]["update"]["sessionUpdate"].clone();
        assert!(
            validator.is_valid(&notification),
            "canonical {kind}: {:?}",
            validator
                .iter_errors(&notification)
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
        );
        notification["params"]["update"]["_meta"]["harn"] = serde_json::json!({});
        assert!(
            !validator.is_valid(&notification),
            "empty identity must not fall through a catch-all for {kind}"
        );
        assert!(
            serde_json::from_value::<generated_rust_binding::ACPTypedSessionUpdate>(
                notification["params"]["update"].clone()
            )
            .is_err(),
            "generated decoder accepted empty {kind}"
        );
    }
}

#[test]
fn typed_session_update_payloads_cover_every_advertised_extension() {
    let payloads = SessionUpdatePayloads::load_for_tests();
    let actual: BTreeSet<_> = payloads
        .variants
        .iter()
        .map(|(kind, _)| kind.as_str())
        .collect();
    let expected: BTreeSet<_> = harn_serve::adapters::acp::HARN_SESSION_UPDATE_EXTENSIONS
        .iter()
        .copied()
        .collect();
    assert!(!expected.is_empty());
    assert_eq!(actual, expected);
}

#[test]
fn native_decoder_preserves_replay_and_required_nulls_and_rejects_bad_identity() {
    let schema: serde_json::Value =
        serde_json::from_str(&protocol_source().read_text(SOURCE).unwrap()).unwrap();
    let validator = jsonschema::draft202012::new(&schema).unwrap();
    for mut notification in adapter_notifications() {
        let update = &mut notification["params"]["update"];
        let kind = update["sessionUpdate"].as_str().unwrap().to_owned();
        update["_meta"]["harn"]["replayed"] = true.into();
        match kind.as_str() {
            "artifact" => update["_meta"]["harn"]["title"] = serde_json::Value::Null,
            "transcript_compacted" => {
                update["_meta"]["harn"]["snapshotAssetId"] = serde_json::Value::Null;
                update["_meta"]["harn"]["compactionPolicy"] = serde_json::Value::Null;
            }
            "reminder_emitted" => {
                update["_meta"]["harn"]["reminder"]["ttlTurns"] = serde_json::Value::Null;
            }
            "worker_update" => {
                update["_meta"]["harn"]["metadata"] = serde_json::Value::Null;
                update["_meta"]["harn"]["audit"] = serde_json::Value::Null;
            }
            _ => {}
        }
        let decoded: generated_rust_binding::ACPTypedSessionUpdate =
            serde_json::from_value(update.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(decoded).unwrap(),
            *update,
            "{kind} replay/null round-trip"
        );
        assert!(
            validator.is_valid(&notification),
            "{kind} nullable producer fields"
        );
    }
    for (kind, field, value) in [
        ("worker_update", "workerId", serde_json::json!(" \t")),
        ("worker_update", "event", serde_json::json!("")),
        ("worker_update", "status", serde_json::json!("")),
        ("skill_activated", "iteration", serde_json::json!(-1)),
        ("skill_activated", "skillName", serde_json::json!(" ")),
        ("stance_transition", "escapeTool", serde_json::json!("")),
    ] {
        let mut notification = adapter_notifications()
            .into_iter()
            .find(|n| n["params"]["update"]["sessionUpdate"] == kind)
            .unwrap();
        notification["params"]["update"]["_meta"]["harn"][field] = value;
        assert!(
            !validator.is_valid(&notification),
            "schema accepted {kind}.{field}"
        );
        assert!(
            serde_json::from_value::<generated_rust_binding::ACPTypedSessionUpdate>(
                notification["params"]["update"].clone()
            )
            .is_err(),
            "decoder accepted {kind}.{field}"
        );
    }
    let mut stance = adapter_notifications()
        .into_iter()
        .find(|n| n["params"]["update"]["sessionUpdate"] == "stance_transition")
        .unwrap();
    stance["params"]["update"]["_meta"]["harn"]
        .as_object_mut()
        .unwrap()
        .remove("escapeTool");
    assert!(!validator.is_valid(&stance));
    assert!(
        serde_json::from_value::<generated_rust_binding::ACPTypedSessionUpdate>(
            stance["params"]["update"].clone()
        )
        .is_err()
    );
    stance["params"]["update"]["_meta"]["harn"]["phase"] = "observing".into();
    assert!(
        validator.is_valid(&stance),
        "escape tools are required only for write-access phases"
    );
    assert!(
        serde_json::from_value::<generated_rust_binding::ACPTypedSessionUpdate>(
            stance["params"]["update"].clone()
        )
        .is_ok()
    );
}

#[test]
fn dump_emits_schema_owned_records_in_every_language() {
    let payloads = SessionUpdatePayloads::load_for_tests();
    let outputs = [
        ("Rust", generate_rust_for_tests(), "pub struct ", ""),
        (
            "Swift",
            generate_swift_for_tests(),
            "public struct ",
            "Harn",
        ),
        (
            "TypeScript",
            generate_typescript_for_tests(),
            "export interface ",
            "",
        ),
        ("Python", generate_python(), "class ", ""),
        ("Go", generate_go(), "type ", ""),
    ];
    for (language, output, declaration, prefix) in outputs {
        for record in &payloads.records {
            assert!(
                output.contains(&format!("{declaration}{prefix}{}", record.name)),
                "{language} missing {}",
                record.name
            );
        }
    }
    let mut schema: serde_json::Value =
        serde_json::from_str(&protocol_source().read_text(SOURCE).unwrap()).unwrap();
    let metadata =
        &mut schema["$defs"]["WorkerUpdate"]["properties"]["_meta"]["properties"]["harn"];
    metadata["properties"]["futureIdentity"] =
        serde_json::json!({"type": "string", "minLength": 1});
    metadata["required"]
        .as_array_mut()
        .unwrap()
        .push("futureIdentity".into());
    let changed = SessionUpdatePayloads::parse(&schema.to_string()).unwrap();
    assert!(
        changed.records.iter().any(|record| record
            .fields
            .iter()
            .any(|field| field.wire_name == "futureIdentity" && field.required)),
        "new schema-required fields must project without editing a field table"
    );
}
