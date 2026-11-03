use super::*;
use crate::wire::anthropic::{encode_request, test_support, AnthropicWireContext};
use crate::wire::prompt_cache::PromptCacheBreakpoint;
use harn_provider_catalog::capabilities::Capabilities;

const SCOPE: Scope = Scope {
    route_epoch: 1,
    credential_epoch: 2,
    policy_epoch: 3,
    beta_epoch: 4,
};

fn initial() -> Value {
    let mut opts = test_support::base_payload();
    opts.messages[0]["content"] = json!("synthetic context ".repeat(512));
    encode_request(
        &opts,
        &AnthropicWireContext {
            capabilities: &Capabilities::default(),
            wire_model: &opts.model,
            prompt_cache: PromptCacheBreakpoint::NotRequested,
            tool_search_extensions: false,
        },
    )
    .body
}

fn response(stop: &str) -> Value {
    json!({
        "type":"message", "role":"assistant", "id":"msg_synthetic",
        "model":"claude-sonnet-4-6", "stop_reason":stop,
        "content":[{"type":"text", "text":"synthetic answer"}],
        "usage":{"input_tokens":100, "output_tokens":10}
    })
}

fn append(full: &Value, reply: &Value, next: Option<Value>) -> Value {
    let mut full = full.clone();
    let messages = full["messages"].as_array_mut().unwrap();
    messages.push(json!({"role":"assistant", "content":reply["content"]}));
    if let Some(next) = next {
        messages.push(next);
    }
    full
}

fn next(full: &Value, reply: &Value) -> Value {
    append(
        full,
        reply,
        Some(json!({"role":"user", "content":"next synthetic question"})),
    )
}

fn seeded(full: &Value, reply: &Value) -> MessageThread {
    let mut thread = MessageThread::default();
    assert!(thread.prepare(SCOPE, full).unwrap().complete(reply));
    thread
}

fn error(code: &str) -> Value {
    let kind = if code == "thread_not_found" {
        "not_found_error"
    } else {
        "invalid_request_error"
    };
    json!({"error":{"type":kind,"details":{"error_code":code}}})
}

#[test]
fn real_encoder_history_reconstructs_from_smaller_continuation() {
    let full = initial();
    let original = full.clone();
    let reply = response("end_turn");
    let mut thread = MessageThread::default();
    let create = thread.prepare(SCOPE, &full).unwrap();
    let mut without_thread = create.body().clone();
    without_thread.as_object_mut().unwrap().remove("thread");
    assert_eq!(without_thread, original);
    assert_eq!(create.required_beta(), Some(BETA));
    assert_eq!(create.body()["thread"], json!({"type":"create"}));
    assert!(create.complete(&reply));

    let full_next = next(&full, &reply);
    let continued = thread.prepare(SCOPE, &full_next).unwrap();
    assert_eq!(continued.mode(), Mode::Continue);
    assert!(continued.body().get("system").is_none());
    assert!(continued.body().get("tools").is_none());
    assert_eq!(
        continued.body()["thread"]["previous_message_id"],
        reply["id"]
    );
    let mut reconstructed = append(&full, &reply, None)["messages"]
        .as_array()
        .unwrap()
        .clone();
    reconstructed.extend(
        continued.body()["messages"]
            .as_array()
            .unwrap()
            .iter()
            .cloned(),
    );
    assert_eq!(json!(reconstructed), full_next["messages"]);
    assert!(
        serde_json::to_vec(continued.body()).unwrap().len() * 4
            < serde_json::to_vec(&full_next).unwrap().len()
    );
    assert_eq!(continued.full_request(), &full_next);
    assert_eq!(full, original);

    // Exercise the next request through Harn's encoder as well: ordinary
    // session history stores a text-only assistant answer as a string.
    let mut opts = test_support::base_payload();
    let first = encode_request(
        &opts,
        &AnthropicWireContext {
            capabilities: &Capabilities::default(),
            wire_model: &opts.model,
            prompt_cache: PromptCacheBreakpoint::NotRequested,
            tool_search_extensions: false,
        },
    )
    .body;
    let mut thread = seeded(&first, &reply);
    opts.messages
        .push(json!({"role":"assistant", "content":"synthetic answer"}));
    opts.messages.push(json!({"role":"user", "content":"next"}));
    let second = encode_request(
        &opts,
        &AnthropicWireContext {
            capabilities: &Capabilities::default(),
            wire_model: &opts.model,
            prompt_cache: PromptCacheBreakpoint::NotRequested,
            tool_search_extensions: false,
        },
    )
    .body;
    let prepared = thread.prepare(SCOPE, &second).unwrap();
    assert_eq!(prepared.mode(), Mode::Continue);
    assert_eq!(
        prepared.body()["messages"],
        json!([{"role":"user", "content":"next"}])
    );
    assert_eq!(prepared.full_request(), &second);

    let mut thread = seeded(&first, &reply);
    let mut edited = second.clone();
    edited["messages"][1]["content"] = json!("edited answer");
    assert_eq!(thread.prepare(SCOPE, &edited).unwrap().mode(), Mode::Create);

    let mut thread = seeded(&first, &reply);
    let mut annotated = second;
    annotated["messages"][1]["content"] =
        json!([{"type":"text", "text":"synthetic answer", "citations":[]}]);
    assert_eq!(
        thread.prepare(SCOPE, &annotated).unwrap().mode(),
        Mode::Create
    );
}

#[test]
fn changed_history_and_fixed_fields_start_a_new_thread_without_editing_history() {
    let full = initial();
    let reply = response("end_turn");
    for field in [
        "system",
        "tools",
        "model",
        "thinking",
        "output_config",
        "tool_choice",
        "cache_control",
        "speed",
    ] {
        let mut thread = seeded(&full, &reply);
        let mut changed = next(&full, &reply);
        changed[field] = if field == "model" {
            json!("other-model")
        } else {
            json!({"synthetic_changed":true})
        };
        let prepared = thread.prepare(SCOPE, &changed).unwrap();
        assert_eq!(prepared.mode(), Mode::Create, "field {field}");
        assert_eq!(prepared.body()["messages"], changed["messages"]);
        assert_eq!(prepared.body()[field], changed[field]);
    }
    for changed in [
        json!([{"role":"user", "content":"summary"}]),
        json!([{"role":"user", "content":"edited"}, {"role":"assistant", "content":reply["content"]}, {"role":"user", "content":"new"}]),
    ] {
        let mut thread = seeded(&full, &reply);
        let mut rewritten = full.clone();
        rewritten["messages"] = changed.clone();
        let prepared = thread.prepare(SCOPE, &rewritten).unwrap();
        assert_eq!(prepared.mode(), Mode::Create);
        assert_eq!(prepared.body()["messages"], changed);
    }
}

#[test]
fn free_fields_keep_the_cursor_but_identity_and_beta_epochs_do_not() {
    let full = initial();
    let reply = response("end_turn");
    for (field, value) in [
        ("max_tokens", json!(128)),
        ("stop_sequences", json!(["STOP"])),
        ("stream", json!(false)),
        ("metadata", json!({"user_id":"synthetic"})),
        ("service_tier", json!("standard_only")),
    ] {
        let mut thread = seeded(&full, &reply);
        let mut changed = next(&full, &reply);
        changed[field] = value;
        assert_eq!(
            thread.prepare(SCOPE, &changed).unwrap().mode(),
            Mode::Continue
        );
    }
    for scope in [
        Scope {
            route_epoch: 9,
            ..SCOPE
        },
        Scope {
            credential_epoch: 9,
            ..SCOPE
        },
        Scope {
            policy_epoch: 9,
            ..SCOPE
        },
        Scope {
            beta_epoch: 9,
            ..SCOPE
        },
    ] {
        let mut thread = seeded(&full, &reply);
        assert_eq!(
            thread.prepare(scope, &next(&full, &reply)).unwrap().mode(),
            Mode::Create
        );
    }
}

#[test]
fn cancellation_or_partial_stream_consumes_the_old_cursor() {
    let full = initial();
    let reply = response("end_turn");
    let mut thread = seeded(&full, &reply);
    let full_next = next(&full, &reply);
    let pending = thread.prepare(SCOPE, &full_next).unwrap();
    assert_eq!(pending.mode(), Mode::Continue);
    drop(pending);
    assert_eq!(
        thread.prepare(SCOPE, &full_next).unwrap().mode(),
        Mode::Create
    );
}

#[test]
fn signed_content_and_parallel_tool_results_remain_in_the_canonical_history() {
    let full = initial();
    let mut reply = response("tool_use");
    reply["content"] = json!([
        {"type":"thinking", "thinking":"synthetic reasoning", "signature":"synthetic signature"},
        {"type":"tool_use", "id":"call_1", "name":"read_file", "input":{"path":"a"}},
        {"type":"tool_use", "id":"call_2", "name":"read_file", "input":{"path":"b"}}
    ]);
    let mut thread = seeded(&full, &reply);
    let results = json!({"role":"user", "content":[
        {"type":"tool_result", "tool_use_id":"call_1", "content":"synthetic a"},
        {"type":"tool_result", "tool_use_id":"call_2", "content":"synthetic b", "is_error":true}
    ]});
    let history = append(&full, &reply, Some(results.clone()));
    let prepared = thread.prepare(SCOPE, &history).unwrap();
    assert_eq!(prepared.mode(), Mode::Continue);
    assert_eq!(prepared.body()["messages"], json!([results]));
    assert_eq!(
        prepared.full_request()["messages"][1]["content"],
        reply["content"]
    );
    assert!(!prepared.body().to_string().contains("synthetic signature"));
    drop(prepared);
    thread.reset();
    let created = thread.prepare(SCOPE, &history).unwrap();
    assert_eq!(created.mode(), Mode::Create);
    assert_eq!(created.body()["messages"][1]["content"], reply["content"]);
}

#[test]
fn only_recognized_pre_generation_errors_get_one_recovery() {
    let full = initial();
    let reply = response("end_turn");
    for (status, code) in [
        (404, "thread_not_found"),
        (400, "thread_already_continued"),
        (400, "thread_continue_in_flight"),
    ] {
        let mut thread = seeded(&full, &reply);
        let full_next = next(&full, &reply);
        let mut prepared = thread.prepare(SCOPE, &full_next).unwrap();
        assert!(prepared.recover_before_generation(status, &error(code)));
        assert_eq!(prepared.mode(), Mode::Create);
        assert_eq!(prepared.body()["messages"], full_next["messages"]);
        assert_eq!(prepared.body()["system"], full_next["system"]);
        assert_eq!(prepared.body()["tools"], full_next["tools"]);
        assert!(!prepared.recover_before_generation(status, &error(code)));
    }
    for (status, body) in [
        (404, error("model_not_found")),
        (404, json!({"error":{"message":"thread_not_found"}})),
        (401, error("thread_not_found")),
        (500, error("thread_not_found")),
        (400, error("thread_fingerprint_mismatch")),
        (404, error("thread_unsupported_request")),
        (
            404,
            json!({"error":{"type":"authentication_error", "details":{"error_code":"thread_not_found"}}}),
        ),
        (
            404,
            json!({"error":{"details":{"error_code":"thread_not_found"}}}),
        ),
    ] {
        let mut thread = seeded(&full, &reply);
        let mut prepared = thread.prepare(SCOPE, &next(&full, &reply)).unwrap();
        assert!(!prepared.recover_before_generation(status, &body));
        assert_eq!(prepared.mode(), Mode::Continue);
    }
    let mut thread = MessageThread::default();
    assert!(!thread
        .prepare(SCOPE, &full)
        .unwrap()
        .recover_before_generation(404, &error("thread_not_found")));
}

#[test]
fn unsupported_feature_disables_threading_only_for_the_same_scope() {
    let full = initial();
    let mut thread = MessageThread::default();
    let mut prepared = thread.prepare(SCOPE, &full).unwrap();
    assert!(prepared.recover_before_generation(400, &error("thread_unsupported_request")));
    assert_eq!(prepared.mode(), Mode::Unthreaded);
    assert_eq!(prepared.body(), &full);
    assert_eq!(prepared.required_beta(), None);
    assert!(!prepared.complete(&response("end_turn")));
    assert_eq!(
        thread.prepare(SCOPE, &full).unwrap().mode(),
        Mode::Unthreaded
    );
    assert_eq!(
        thread
            .prepare(
                Scope {
                    route_epoch: 8,
                    ..SCOPE
                },
                &full
            )
            .unwrap()
            .mode(),
        Mode::Create
    );
}

#[test]
fn incomplete_refused_fallback_or_malformed_responses_never_supply_a_cursor() {
    let full = initial();
    let ordinary = response("end_turn");
    let mut variants = Vec::new();
    for stop in ["refusal", "max_tokens", "stop_sequence", "unknown"] {
        variants.push(response(stop));
    }
    for (field, value) in [
        ("id", json!(" ")),
        ("model", json!("fallback-model")),
        ("role", json!("user")),
        ("content", Value::Null),
        ("content", json!(["malformed block"])),
        ("content", json!([{"missing":"type"}])),
        ("type", json!("error")),
        ("stop_reason", Value::Null),
    ] {
        let mut reply = ordinary.clone();
        reply[field] = value;
        variants.push(reply);
    }
    for iterations in [
        json!([{"type":"fallback_message"}]),
        json!([{"future_iteration":true}]),
        json!({}),
    ] {
        let mut reply = ordinary.clone();
        reply["usage"]["iterations"] = iterations;
        variants.push(reply);
    }
    for reply in variants {
        let mut thread = MessageThread::default();
        assert!(!thread.prepare(SCOPE, &full).unwrap().complete(&reply));
        assert_eq!(
            thread
                .prepare(SCOPE, &next(&full, &ordinary))
                .unwrap()
                .mode(),
            Mode::Create
        );
    }
}

#[test]
fn paused_turn_accepts_empty_resume_but_refuses_a_new_user_message() {
    let full = initial();
    let reply = response("pause_turn");
    let mut thread = seeded(&full, &reply);
    let paused = append(&full, &reply, None);
    let prepared = thread.prepare(SCOPE, &paused).unwrap();
    assert_eq!(prepared.mode(), Mode::Continue);
    assert_eq!(prepared.body()["messages"], json!([]));
    drop(prepared);
    let mut thread = seeded(&full, &reply);
    assert!(matches!(
        thread.prepare(SCOPE, &next(&full, &reply)),
        Err(PrepareError::TurnPaused)
    ));
    let mut thread = seeded(&full, &response("end_turn"));
    assert!(matches!(
        thread.prepare(SCOPE, &paused),
        Err(PrepareError::NoNewMessages)
    ));
}

#[test]
fn invalid_or_opaque_request_features_fail_before_any_wire_transformation() {
    for body in [
        Value::Null,
        json!({"model":"m", "messages":[]}),
        json!({"model":"m", "messages":[{"role":"system", "content":"x"}]}),
        json!({"model":"m", "messages":[{"role":"user"}]}),
    ] {
        let mut thread = MessageThread::default();
        assert!(matches!(
            thread.prepare(SCOPE, &body),
            Err(PrepareError::InvalidRequest)
        ));
    }
    for field in [
        "compaction",
        "context_management",
        "mcp_servers",
        "container",
        "fallbacks",
        "future_field",
    ] {
        let mut thread = MessageThread::default();
        let mut body = initial();
        body[field] = json!({});
        assert!(matches!(
            thread.prepare(SCOPE, &body),
            Err(PrepareError::UnsupportedField)
        ));
    }
    let mut thread = MessageThread::default();
    let mut body = initial();
    body["thread"] = json!({"type":"continue", "previous_message_id":"injected"});
    assert!(matches!(
        thread.prepare(SCOPE, &body),
        Err(PrepareError::AlreadyThreaded)
    ));
}
