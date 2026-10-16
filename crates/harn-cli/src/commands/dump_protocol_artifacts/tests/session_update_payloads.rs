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

fn typed_notifications() -> Vec<serde_json::Value> {
    let mut notifications = adapter_notifications();
    // These updates come from session::refresh_advertised_commands,
    // bridge::send_log and live_clients::write_live_client_operation rather
    // than AgentEvent, so they are absent from the event-emission fixture.
    for update in [
        serde_json::json!({"sessionUpdate": "available_commands_update", "availableCommands": []}),
        serde_json::json!({"sessionUpdate": "log", "_meta": {"harn": {"level": "info", "message": "fixture log"}}}),
        serde_json::json!({"sessionUpdate": "live_session_client", "_meta": {"harn": {"action": "attached", "state": null}}}),
    ] {
        notifications.push(serde_json::json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "fixture-session", "update": update}}));
    }
    notifications
}

#[test]
fn generated_session_updates_round_trip_the_adapter_fixture() {
    let notifications = typed_notifications();
    let kinds: BTreeSet<_> = notifications
        .iter()
        .map(|notification| {
            notification["params"]["update"]["sessionUpdate"]
                .as_str()
                .expect("kind")
        })
        .collect();
    assert_eq!(
        kinds,
        harn_serve::adapters::acp::HARN_SESSION_UPDATE_EXTENSIONS
            .iter()
            .copied()
            .collect(),
        "typed fixtures must exercise every advertised kind"
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
fn generated_session_updates_refuse_each_missing_required_field() {
    fn required_paths(
        shape: &serde_json::Value,
        value: &serde_json::Value,
        prefix: &str,
        paths: &mut Vec<String>,
    ) {
        let Some(properties) = shape["properties"].as_object() else {
            return;
        };
        for (key, field) in properties {
            let path = format!("{prefix}/{key}");
            if shape["required"]
                .as_array()
                .is_some_and(|fields| fields.iter().any(|name| name == key))
            {
                assert!(value.get(key).is_some(), "fixture lacks {path}");
                paths.push(path.clone());
            }
            if let Some(child) = value.get(key) {
                required_paths(field, child, &path, paths);
            }
        }
    }
    let schema: serde_json::Value =
        serde_json::from_str(&protocol_source().read_text(SOURCE).unwrap()).unwrap();
    let mut checked = BTreeSet::new();
    for notification in typed_notifications() {
        let update = &notification["params"]["update"];
        let kind = update["sessionUpdate"].as_str().unwrap();
        let shape = schema["$defs"]
            .as_object()
            .unwrap()
            .values()
            .find(|shape| shape["properties"]["sessionUpdate"]["const"] == kind)
            .unwrap();
        let mut paths = Vec::new();
        required_paths(shape, update, "", &mut paths);
        assert!(!paths.is_empty(), "{kind} required-field census is empty");
        for path in paths {
            let mut missing = update.clone();
            let (parent, key) = path.rsplit_once('/').unwrap();
            assert!(missing
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(key)
                .is_some());
            let error =
                serde_json::from_value::<generated_rust_binding::ACPTypedSessionUpdate>(missing)
                    .unwrap_err();
            let expected = if path == "/sessionUpdate" {
                "unknown typed session update".to_owned()
            } else {
                format!(
                    "session update {} is required",
                    path.trim_start_matches('/').replace('/', ".")
                )
            };
            assert_eq!(error.to_string(), expected, "{kind}: {path}");
            checked.insert((kind.to_owned(), path));
        }
    }
    assert_eq!(
        checked
            .iter()
            .map(|(kind, _)| kind)
            .collect::<BTreeSet<_>>()
            .len(),
        harn_serve::adapters::acp::HARN_SESSION_UPDATE_EXTENSIONS.len()
    );
}

#[test]
fn generated_rust_session_update_dispatch_contains_only_variant_routes() {
    let output = generate_rust_for_tests();
    let decoder = output
        .split("impl<'de> Deserialize<'de> for ACPTypedSessionUpdate {")
        .nth(1)
        .unwrap()
        .split("\n}\n")
        .next()
        .unwrap();
    assert_eq!(decoder.matches("match ").count(), 1);
    assert_eq!(decoder.matches("validate_session_update(").count(), 1);
    assert!(!decoder.contains("value.pointer("));
    assert!(decoder.lines().count() <= SessionUpdatePayloads::load_for_tests().variants.len() + 12);
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
                update["_meta"]["harn"]["classification"] = serde_json::json!({
                    "status": "applied",
                    "confidence_floor": 0.8,
                    "rounds": 1,
                    "budget_bytes": 512,
                    "result_bytes": 256,
                    "budget_met": true,
                    "summary_applied": false,
                    "decisions": [],
                    "fallback_reason": null
                });
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
    for (kind, field, value, reason) in [
        (
            "worker_update",
            "workerId",
            serde_json::json!(" \t"),
            "must not be blank",
        ),
        (
            "worker_update",
            "event",
            serde_json::json!(""),
            "is too short",
        ),
        (
            "worker_update",
            "status",
            serde_json::json!(""),
            "is too short",
        ),
        (
            "skill_activated",
            "iteration",
            serde_json::json!(-1),
            "is below its minimum",
        ),
        (
            "skill_activated",
            "skillName",
            serde_json::json!(" "),
            "must not be blank",
        ),
        (
            "stance_transition",
            "escapeTool",
            serde_json::json!(""),
            "is too short",
        ),
        (
            "reminder_emitted",
            "reminder/renderedRole",
            serde_json::json!("unknown-role"),
            "has an unknown value",
        ),
    ] {
        let mut notification = adapter_notifications()
            .into_iter()
            .find(|n| n["params"]["update"]["sessionUpdate"] == kind)
            .unwrap();
        *notification
            .pointer_mut(&format!("/params/update/_meta/harn/{field}"))
            .unwrap() = value;
        assert!(
            !validator.is_valid(&notification),
            "schema accepted {kind}.{field}"
        );
        assert_eq!(
            serde_json::from_value::<generated_rust_binding::ACPTypedSessionUpdate>(
                notification["params"]["update"].clone()
            )
            .unwrap_err()
            .to_string(),
            format!(
                "session update _meta.harn.{} {reason}",
                field.replace('/', ".")
            ),
            "{kind}.{field} diagnostic"
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
    assert_eq!(
        serde_json::from_value::<generated_rust_binding::ACPTypedSessionUpdate>(
            stance["params"]["update"].clone()
        )
        .unwrap_err()
        .to_string(),
        "session update _meta.harn.escapeTool is required for this phase"
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
