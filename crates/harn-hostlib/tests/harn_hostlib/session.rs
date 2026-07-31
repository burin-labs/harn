use harn_hostlib::session::{
    SessionCapability, APPEND_BUILTIN, GET_BUILTIN, LIST_BUILTIN, OPEN_BUILTIN,
    SEARCH_HYBRID_BUILTIN, UPDATE_BUILTIN,
};
use harn_hostlib::{BuiltinRegistry, HostlibCapability};
use harn_vm::VmValue;
use serde_json::json;

async fn invoke(registry: &BuiltinRegistry, name: &str, request: serde_json::Value) -> VmValue {
    let builtin = registry
        .find_async(name)
        .unwrap_or_else(|| panic!("{name} must be registered"));
    (builtin.handler)(vec![harn_vm::json_to_vm_value(&request)])
        .await
        .unwrap_or_else(|error| panic!("{name} failed: {error}"))
}

fn list_len(value: Option<&VmValue>) -> Option<usize> {
    match value {
        Some(VmValue::List(items)) => Some(items.len()),
        _ => None,
    }
}

#[tokio::test]
async fn hostlib_session_surface_round_trips_one_canonical_store() {
    let temp = tempfile::tempdir().expect("temp root");
    let root = temp.path().to_string_lossy();
    let mut registry = BuiltinRegistry::new();
    SessionCapability::default().register_builtins(&mut registry);

    let opened = invoke(
        &registry,
        OPEN_BUILTIN,
        json!({
            "root": root,
            "id": "hostlib-session",
            "title": "Hostlib session",
            "session_type": "user",
        }),
    )
    .await;
    let opened = opened.as_dict().expect("open response");
    assert_eq!(
        opened.get("id").map(VmValue::display).as_deref(),
        Some("hostlib-session")
    );
    let updated = invoke(
        &registry,
        UPDATE_BUILTIN,
        json!({
            "root": root,
            "session_id": "hostlib-session",
            "model": "model-v2",
            "usage_input": 12,
            "usage_output": 5,
        }),
    )
    .await;
    let updated = updated.as_dict().expect("update response");
    assert_eq!(
        updated.get("model").map(VmValue::display).as_deref(),
        Some("model-v2")
    );

    invoke(
        &registry,
        APPEND_BUILTIN,
        json!({
            "root": root,
            "session_id": "hostlib-session",
            "event": {
                "kind": {"kind": "message"},
                "payload": {"text": "canonical hostlib marker"},
            },
        }),
    )
    .await;

    let listed = invoke(&registry, LIST_BUILTIN, json!({"root": root})).await;
    let listed = listed.as_dict().expect("list response");
    assert_eq!(list_len(listed.get("sessions")), Some(1));

    let fetched = invoke(
        &registry,
        GET_BUILTIN,
        json!({"root": root, "session_id": "hostlib-session"}),
    )
    .await;
    let fetched = fetched.as_dict().expect("get response");
    assert_eq!(list_len(fetched.get("events")), Some(1));

    let search = invoke(
        &registry,
        SEARCH_HYBRID_BUILTIN,
        json!({"root": root, "query": "hostlib marker"}),
    )
    .await;
    let search = search.as_dict().expect("search response");
    assert_eq!(
        search
            .get("effective_mode")
            .map(VmValue::display)
            .as_deref(),
        Some("fts")
    );
    assert_eq!(list_len(search.get("hits")), Some(1));
}
