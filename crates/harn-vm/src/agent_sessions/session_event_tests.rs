use super::*;

fn stored(kind: SessionEventKind, payload: Value) -> StoredEvent {
    StoredEvent {
        event_id: 1,
        session_id: "session".to_string(),
        tenant_id: None,
        parent_event_id: None,
        actor: None,
        kind,
        payload,
        tags: Vec::new(),
        headers: BTreeMap::new(),
        ts_ms: 0,
        ts: "2026-10-13T00:00:00Z".to_string(),
        record_hash: "hash".to_string(),
        prev_hash: None,
        signed_by: None,
    }
}

/// Payload shaped exactly as the session store persisted an `llm_call` before
/// schema 2, copied from a real headless run rather than invented here.
fn schema_one_llm_call() -> Value {
    serde_json::json!({
        "transcript_event": {
            "blocks": [{"text": "LLM call completed", "type": "text"}],
            "id": "019fc7e6-71f4-7583-8664-55fdb52f962e",
            "kind": "llm_call",
            "metadata": {
                "cache_read_tokens": 0,
                "cache_write_tokens": 10948,
                "canonical_stop_reason": "end_turn",
                "cost_usd": 0.002872,
                "input_tokens": 10951,
                "model": "gpt-5.6-luna",
                "output_tokens": 112,
                "provider": "openai",
                "provider_stop_reason": "completed"
            },
            "role": "assistant",
            "text": "LLM call completed"
        }
    })
}

#[test]
fn a_schema_one_row_decodes_to_the_same_typed_facts() {
    let event = stored(
        SessionEventKind::Custom {
            custom_type: "llm_call".to_string(),
        },
        schema_one_llm_call(),
    );
    let record = transcript_record(&event).expect("schema-1 transcript row");
    assert_eq!(record.kind(), Some(&TranscriptKind::LlmCall));
    let metadata = record.metadata();
    assert_eq!(nonblank(&metadata.model).as_deref(), Some("gpt-5.6-luna"));
    assert_eq!(nonblank(&metadata.provider).as_deref(), Some("openai"));
    assert_eq!(integer(&metadata.input_tokens), Some(10951));
    assert_eq!(integer(&metadata.output_tokens), Some(112));
    assert_eq!(integer(&metadata.cache_write_tokens), Some(10948));
    assert_eq!(float(&metadata.cost_usd), Some(0.002872));
    assert_eq!(record.text().as_deref(), Some("LLM call completed"));
    assert_eq!(record.role().as_deref(), Some("assistant"));
}

/// Decoding is lossless: fields the runtime does not read, numbers exactly as
/// produced, and the envelope's own fields all survive a write.
#[test]
fn a_transcript_event_round_trips_every_field_it_does_not_read() {
    let original = schema_one_llm_call()["transcript_event"].clone();
    let record = TranscriptRecord::new(original.clone(), None);
    let written = SessionEventPayload::Transcript(record).to_json();
    assert_eq!(written["record"], "transcript");
    assert_eq!(written["transcript_event"], original);
    assert!(written.get("raw_message").is_none());
}

/// The terminal writer records `"error": null` for a clean finish; a typed
/// `Option` must not erase that on write.
#[test]
fn explicit_nulls_survive_a_write() {
    let event = serde_json::json!({
        "kind": "agent_run_terminal",
        "text": null,
        "metadata": {"final_status": "done", "error": null, "terminal_class": null}
    });
    let record = TranscriptRecord::new(event.clone(), None);
    assert_eq!(record.metadata().error, None);
    let written = SessionEventPayload::Transcript(record).to_json();
    assert_eq!(written["transcript_event"], event);
}

#[test]
fn metadata_whose_known_fields_are_mistyped_is_kept_rather_than_refused() {
    let event = serde_json::json!({
        "kind": "llm_call",
        "metadata": {
            "model": 42, "input_tokens": "many", "extra": true,
            "cost_usd": 0.25, "provider": "openai", "output_tokens": 12
        }
    });
    let record = TranscriptRecord::new(event.clone(), None);
    assert_eq!(record.metadata().model, None);
    assert_eq!(integer(&record.metadata().input_tokens), None);
    assert_eq!(float(&record.metadata().cost_usd), Some(0.25));
    assert_eq!(
        nonblank(&record.metadata().provider).as_deref(),
        Some("openai")
    );
    assert_eq!(integer(&record.metadata().output_tokens), Some(12));
    let written = SessionEventPayload::Transcript(record).to_json();
    assert_eq!(written["transcript_event"], event);
}

#[test]
fn a_mistyped_envelope_field_does_not_hide_valid_metadata() {
    let event = serde_json::json!({
        "kind": "llm_call", "role": 42, "text": "completed",
        "metadata": {"cost_usd": 0.25}
    });
    let record = TranscriptRecord::new(event.clone(), None);
    assert_eq!(record.kind(), Some(&TranscriptKind::LlmCall));
    assert_eq!(record.text().as_deref(), Some("completed"));
    assert_eq!(float(&record.metadata().cost_usd), Some(0.25));
    assert_eq!(
        SessionEventPayload::Transcript(record).to_json()["transcript_event"],
        event
    );
}

#[test]
fn absent_and_blank_facts_read_as_missing_rather_than_empty() {
    let record = TranscriptRecord::new(
        serde_json::json!({"text": "   ", "metadata": {"model": ""}}),
        None,
    );
    // A whitespace-only or empty string is the emitter declining to say,
    // not a value. Callers that fall back to another source depend on this
    // being `None` rather than `Some("")`.
    assert_eq!(nonblank(&record.metadata().model), None);
    assert_eq!(record.text(), None);
    assert_eq!(integer(&record.metadata().input_tokens), None);
    assert_eq!(float(&record.metadata().cost_usd), None);
    assert!(!record.audit_is_error());
}

#[test]
fn token_counts_serialized_as_floats_still_read_as_integers() {
    // Some providers report usage as JSON floats. Rejecting those would
    // silently drop the whole call's usage from an aggregate.
    let record = TranscriptRecord::new(
        serde_json::json!({"kind": "llm_call", "metadata": {"input_tokens": 1024.0}}),
        None,
    );
    assert_eq!(integer(&record.metadata().input_tokens), Some(1024));
    let written = SessionEventPayload::Transcript(record).to_json();
    assert_eq!(
        written["transcript_event"]["metadata"]["input_tokens"],
        serde_json::json!(1024.0)
    );
}

#[test]
fn text_prefers_the_transcript_envelope_over_the_raw_message() {
    let record = TranscriptRecord::new(
        serde_json::json!({"text": "envelope"}),
        Some(serde_json::json!({"content": "raw"})),
    );
    assert_eq!(record.text().as_deref(), Some("envelope"));

    // With no envelope text the raw message is the only source left, and
    // dropping to it must not require the caller to know the order.
    let raw_only = TranscriptRecord::new(
        serde_json::json!({}),
        Some(serde_json::json!({"content": "raw"})),
    );
    assert_eq!(raw_only.text().as_deref(), Some("raw"));
}

/// The store discriminator is derived from the typed kind, once, for every
/// path a row can be written by.
#[test]
fn store_kinds_follow_the_typed_transcript_kind() {
    let kind = |event: Value, raw: Option<Value>| {
        SessionEventPayload::Transcript(TranscriptRecord::new(event, raw)).store_kind()
    };
    let raw = || Some(serde_json::json!({"role": "user", "content": "hi"}));
    assert_eq!(
        kind(serde_json::json!({"kind": "message"}), raw()),
        SessionEventKind::Message
    );
    assert_eq!(
        kind(serde_json::json!({}), raw()),
        SessionEventKind::Message
    );
    assert_eq!(
        kind(serde_json::json!({}), None),
        SessionEventKind::Custom {
            custom_type: "transcript_event".to_string()
        }
    );
    assert_eq!(
        kind(serde_json::json!({"kind": "message"}), None),
        SessionEventKind::Custom {
            custom_type: "message".to_string()
        }
    );
    assert_eq!(
        kind(serde_json::json!({"kind": "tool_result"}), None),
        SessionEventKind::ToolResult
    );
    assert_eq!(
        kind(serde_json::json!({"kind": "plan_document"}), None),
        SessionEventKind::Plan
    );
    assert_eq!(
        kind(serde_json::json!({"kind": "permission_decision"}), None),
        SessionEventKind::PermissionDecision
    );
    assert_eq!(
        kind(serde_json::json!({"kind": "control"}), None),
        SessionEventKind::Control
    );
    assert_eq!(
        kind(serde_json::json!({"kind": "llm_call"}), None),
        SessionEventKind::Custom {
            custom_type: "llm_call".to_string()
        }
    );
    // A provider message that only carries a tool call files as one.
    assert_eq!(
        kind(
            serde_json::json!({"kind": "message", "visibility": "internal"}),
            Some(serde_json::json!({"role": "assistant", "tool_calls": [{"id": "call-1"}]})),
        ),
        SessionEventKind::ToolCall
    );
}

#[test]
fn every_named_transcript_kind_round_trips_through_its_wire_name() {
    for (kind, name) in TranscriptKind::NAMED {
        assert_eq!(kind.as_str(), name);
        assert_eq!(TranscriptKind::from_wire(name), kind);
    }
    assert_eq!(
        TranscriptKind::from_wire("script_defined"),
        TranscriptKind::Other("script_defined".to_string())
    );
}

#[test]
fn schema_one_compaction_and_removal_rows_decode_by_shape() {
    let compaction = stored(
        SessionEventKind::Compaction,
        serde_json::json!({"messages": [{"role": "user"}], "summary": "s", "source_event_ids": ["a"]}),
    );
    assert!(matches!(
        SessionEventPayload::decode(&compaction),
        Some(SessionEventPayload::Compaction(record)) if record.summary.as_deref() == Some("s")
    ));
    let removed = stored(
        SessionEventKind::Custom {
            custom_type: "message_removed".to_string(),
        },
        serde_json::json!({"source_event_id": "a", "raw_message": {"role": "user"}}),
    );
    assert!(matches!(
        SessionEventPayload::decode(&removed),
        Some(SessionEventPayload::MessageRemoved(record)) if record.source_event_id == "a"
    ));
    // A compaction audit row is a transcript event, not a history rewrite.
    let audit = stored(
        SessionEventKind::Compaction,
        serde_json::json!({"transcript_event": {"kind": "compaction"}}),
    );
    assert!(matches!(
        SessionEventPayload::decode(&audit),
        Some(SessionEventPayload::Transcript(_))
    ));
    // Another surface's custom row is not the journal's.
    let foreign = stored(
        SessionEventKind::Custom {
            custom_type: "note".to_string(),
        },
        serde_json::json!({"text": "hi"}),
    );
    assert_eq!(SessionEventPayload::decode(&foreign), None);
}

#[test]
fn tool_facts_read_across_the_envelope_and_the_provider_message() {
    let result = TranscriptRecord::new(
        serde_json::json!({"kind": "tool_result", "text": "boom"}),
        Some(serde_json::json!({
            "role": "tool",
            "name": "edit",
            "content": "boom",
            "is_error": true,
            "_harn": {"tool_call_id": "call-1", "data": {"verification": {"status": "passed"}}},
        })),
    );
    assert_eq!(result.tool_name().as_deref(), Some("edit"));
    assert!(result.is_error());
    assert!(!result.audit_is_error());
    assert_eq!(
        result.tool_call_id(&BTreeMap::new()).as_deref(),
        Some("call-1")
    );
    assert_eq!(result.tool_verification().unwrap()["status"], "passed");
    assert_eq!(result.tool_output(), Some(serde_json::json!("boom")));

    let call = TranscriptRecord::new(
        serde_json::json!({"kind": "tool_call", "metadata": {"raw_input": {"a": 1}}}),
        Some(
            serde_json::json!({"tool_calls": [{"id": "c", "name": "add", "arguments": {"a": 2}}]}),
        ),
    );
    assert_eq!(call.tool_name().as_deref(), Some("add"));
    assert_eq!(call.tool_input(), Some(serde_json::json!({"a": 2})));
    assert_eq!(call.declared_tool_name(), None);
    assert_eq!(call.provider_tool_call("c").unwrap()["name"], "add");
}
