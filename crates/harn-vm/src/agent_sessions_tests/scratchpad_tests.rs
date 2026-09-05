use super::*;

#[test]
fn scratchpad_round_trips_through_session_state_snapshot_and_transcript_metadata() {
    reset_session_store();
    let id = open_or_create(Some("scratchpad-session".into()));

    let version = set_scratchpad(
        &id,
        scratchpad_value("remember this"),
        "test",
        Some("seed".into()),
        serde_json::json!({"iteration": 1}),
    )
    .expect("set scratchpad");

    assert_eq!(version, 1);
    assert_eq!(scratchpad_version(&id), Some(1));
    assert_eq!(
        scratchpad(&id).and_then(|value| crate::llm::helpers::vm_value_to_json(&value)
            .pointer("/facts/0/text")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)),
        Some("remember this".to_string())
    );
    let snapshot_json =
        crate::llm::helpers::vm_value_to_json(&snapshot(&id).expect("session snapshot"));
    assert_eq!(snapshot_json["scratchpad_version"], 1);
    assert_eq!(
        snapshot_json["scratchpad"]["facts"][0]["source_ref"],
        "turn:1"
    );
    assert_eq!(
        snapshot_json["metadata"]["agent_scratchpad"]["facts"][0]["text"],
        "remember this"
    );
    let events = events_by_kind_json(&id, "agent_scratchpad");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["metadata"]["action"], "set");
    assert_eq!(events[0]["metadata"]["counts"]["facts"], 1);

    let cleared = clear_scratchpad(&id, "test", Some("done".into()), serde_json::json!({}))
        .expect("clear scratchpad");
    assert_eq!(cleared, 2);
    assert!(scratchpad(&id).is_none());
    let snapshot_json =
        crate::llm::helpers::vm_value_to_json(&snapshot(&id).expect("session snapshot"));
    assert!(snapshot_json["scratchpad"].is_null());
    assert_eq!(snapshot_json["scratchpad_version"], 2);

    reset_session_store();
}

#[test]
fn fork_inherits_scratchpad_but_reset_clears_it() {
    reset_session_store();
    let parent = open_or_create(Some("scratchpad-parent".into()));
    set_scratchpad(
        &parent,
        scratchpad_value("carry forward"),
        "test",
        None,
        serde_json::json!({}),
    )
    .expect("set scratchpad");

    let child = fork(&parent, Some("scratchpad-child".into())).expect("fork");
    assert_eq!(scratchpad_version(&child), Some(1));
    assert_eq!(
        scratchpad(&child).and_then(|value| crate::llm::helpers::vm_value_to_json(&value)
            .pointer("/facts/0/text")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)),
        Some("carry forward".to_string())
    );
    set_scratchpad(
        &child,
        scratchpad_value("child-only"),
        "test",
        None,
        serde_json::json!({}),
    )
    .expect("update child");
    assert_eq!(
        scratchpad(&parent).and_then(|value| crate::llm::helpers::vm_value_to_json(&value)
            .pointer("/facts/0/text")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)),
        Some("carry forward".to_string())
    );

    assert!(reset_transcript(&child));
    assert!(scratchpad(&child).is_none());
    assert_eq!(scratchpad_version(&child), Some(0));

    reset_session_store();
}

#[test]
fn scratchpad_rejects_non_dict_and_oversized_values() {
    reset_session_store();
    let id = open_or_create(Some("scratchpad-validation".into()));

    let non_dict_error = set_scratchpad(
        &id,
        VmValue::String(arcstr::ArcStr::from("nope")),
        "test",
        None,
        serde_json::json!({}),
    )
    .unwrap_err();
    assert!(non_dict_error.contains("must be a dict"));

    let oversized = VmValue::dict(crate::value::DictMap::from_iter([(
        "notes".to_string(),
        VmValue::String(arcstr::ArcStr::from("x".repeat(MAX_SCRATCHPAD_BYTES + 1))),
    )]));
    let oversized_error =
        set_scratchpad(&id, oversized, "test", None, serde_json::json!({})).unwrap_err();
    assert!(
        oversized_error.contains("max is"),
        "oversized scratchpad should name the cap: {oversized_error}"
    );

    reset_session_store();
}
