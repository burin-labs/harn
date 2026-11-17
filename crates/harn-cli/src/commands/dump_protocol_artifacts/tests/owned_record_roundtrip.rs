use super::*;

#[test]
fn generated_timeline_bindings_preserve_runtime_records_and_defaults() {
    use generated_rust_binding::{
        HarnSessionTimelineQuery, HarnSessionTimelineSnapshot, HarnSessionTimelineUpdate,
    };
    let fixture = super::super::timeline_records::round_trip_fixture().unwrap();
    let snapshot: HarnSessionTimelineSnapshot =
        serde_json::from_value(fixture["snapshot"].clone()).unwrap();
    let update: HarnSessionTimelineUpdate =
        serde_json::from_value(fixture["update"].clone()).unwrap();
    assert_eq!(snapshot.nodes.len(), 1);
    assert_eq!(snapshot.nodes[0].kind, "future_kind");
    assert_eq!(serde_json::to_value(snapshot).unwrap(), fixture["snapshot"]);
    assert_eq!(serde_json::to_value(update).unwrap(), fixture["update"]);
    let query: HarnSessionTimelineQuery = serde_json::from_value(json!({})).unwrap();
    let runtime: harn_vm::session_timeline::SessionTimelineQuery =
        serde_json::from_value(json!({})).unwrap();
    assert_eq!(
        serde_json::to_value(query).unwrap(),
        serde_json::to_value(runtime).unwrap()
    );
    let mut invalid = fixture["snapshot"].clone();
    invalid["nodes"][0]["references"][0]["eventId"] = json!("unmeasured");
    assert!(serde_json::from_value::<HarnSessionTimelineSnapshot>(invalid).is_err());
}

#[test]
fn generated_tool_annotations_preserve_owned_safety_hints() {
    let fixture: serde_json::Value =
        serde_json::from_str(&generate_round_trip_fixture().unwrap()).unwrap();
    let expected = fixture["harnToolAnnotations"].clone();
    let annotations: generated_rust_binding::HarnToolAnnotations =
        serde_json::from_value(expected.clone()).unwrap();
    assert_eq!(annotations.read_only_hint, Some(false));
    assert_eq!(annotations.destructive_hint, Some(true));
    assert_eq!(annotations.idempotent_hint, Some(false));
    assert_eq!(annotations.open_world_hint, Some(true));
    assert_eq!(serde_json::to_value(annotations).unwrap(), expected);
}
