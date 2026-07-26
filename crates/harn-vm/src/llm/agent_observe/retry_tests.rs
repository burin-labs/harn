use super::*;
use crate::value::{ErrorCategory, VmError, VmValue};

fn thrown(s: &str) -> VmError {
    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(s)))
}

fn categorized(msg: &str, category: ErrorCategory) -> VmError {
    VmError::CategorizedError {
        message: msg.to_string(),
        category,
    }
}

fn set_env_for_test(key: &str, value: Option<&str>) -> Option<String> {
    let previous = std::env::var(key).ok();
    match value {
        Some(value) => std::env::set_var(key, value),
        None => std::env::remove_var(key),
    }
    previous
}

fn restore_env_for_test(key: &str, previous: Option<String>) {
    match previous {
        Some(value) => std::env::set_var(key, value),
        None => std::env::remove_var(key),
    }
}

fn temp_transcript_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!("{label}-{}", uuid::Uuid::now_v7()))
}

// ----- L0 governor throttle detection (VmError -> ThrottleSignal) --------

#[test]
fn governor_detects_rate_limit_and_overload_from_runtime_errors() {
    use crate::llm::rate_governor::ThrottleSignal;
    // A 429 rate-limit error (however carried) → RateLimit429.
    assert_eq!(
        governor_throttle_signal_for_error(&thrown(
            "anthropic HTTP 429 [rate_limited]: rate_limit_error"
        )),
        Some(ThrottleSignal::RateLimit429)
    );
    assert_eq!(
        governor_throttle_signal_for_error(&categorized(
            "provider rate limit exceeded",
            ErrorCategory::RateLimit
        )),
        Some(ThrottleSignal::RateLimit429)
    );
    // A provider overload (529/503 / overloaded_error) → Overloaded.
    assert_eq!(
        governor_throttle_signal_for_error(&categorized(
            "anthropic overloaded_error",
            ErrorCategory::Overloaded
        )),
        Some(ThrottleSignal::Overloaded)
    );
    // Non-throttle failures carry no governor signal (the network breaker /
    // retry path owns those).
    assert_eq!(
        governor_throttle_signal_for_error(&categorized(
            "connection reset by peer",
            ErrorCategory::TransientNetwork
        )),
        None
    );
    assert_eq!(
        governor_throttle_signal_for_error(&thrown("some generic 500 server_error")),
        None
    );
}

// ----- runtime tool_format fallback (native -> text) ---------------------

#[test]
fn native_tool_channel_failure_matches_ollama_500_parser_signature() {
    // The documented Ollama leak: the server-side qwen3 tool-call extractor
    // EOFs and the server returns HTTP 500 instead of degrading to content.
    assert!(is_native_tool_channel_failure(&thrown(
        "[http_error] ollama 500: tool call parser hit unexpected EOF while parsing"
    )));
    // The same shape carried as a CategorizedError.
    assert!(is_native_tool_channel_failure(&categorized(
        "status 500: server tool extractor failed to parse tool_calls",
        ErrorCategory::ServerError
    )));
    // A stream-cut EOF in the tool-call extractor (no explicit status code).
    assert!(is_native_tool_channel_failure(&thrown(
        "error decoding stream: EOF while parsing a tool call"
    )));
}

#[test]
fn native_tool_channel_failure_ignores_generic_and_keyword_only_errors() {
    // A plain 503 with no tool-parser fingerprint stays an ordinary
    // transient retry — retrying native is correct when the LINK hiccuped.
    assert!(!is_native_tool_channel_failure(&thrown(
        "service unavailable (503): upstream temporarily overloaded"
    )));
    // A 429 rate limit is never a tool-channel failure.
    assert!(!is_native_tool_channel_failure(&thrown(
        "[rate_limited] too many requests"
    )));
    // The word "tool" alone, without a 5xx/EOF, must not trip the degrade
    // (e.g. a 400 complaining about a malformed tool schema).
    assert!(!is_native_tool_channel_failure(&thrown(
        "bad request: tool schema invalid"
    )));
    // A 500 with no tool-parser fingerprint is a generic server error.
    assert!(!is_native_tool_channel_failure(&thrown(
        "[http_error] 500 internal server error"
    )));
}

#[test]
fn billed_noncommittal_throw_matches_vanish_and_function_call_refusal() {
    // (1) The billed-noncommittal contract violation (reasoning-channel-only
    // vanish): the canonical cheap-model vanishing-call signature.
    assert!(is_billed_noncommittal_throw(&thrown(
        "provider deepinfra model openai/gpt-oss-120b returned billed output \
             (completion_tokens=86) with no dispatchable tool call or answer \
             (upstream contract violation): the model finished cleanly but committed \
             neither a tool call nor visible text."
    )));
    // Also matches when re-wrapped as a categorized error.
    assert!(is_billed_noncommittal_throw(&categorized(
        "model m returned billed output (completion_tokens=5) with no dispatchable \
             tool call or answer (upstream contract violation)",
        ErrorCategory::Generic,
    )));
    // (2) The SambaNova native function-call protocol refusal (HTTP 400). It
    // is a 4xx, so `is_native_tool_channel_failure` (5xx/EOF only) does NOT
    // match it, but it is unambiguously a broken native tool channel.
    assert!(is_billed_noncommittal_throw(&thrown(
        "sambanova HTTP 400 Bad Request [invalid_request]: Model started a \
             function call but did not complete it."
    )));
    // 5xx-only path stays separate from the 400 refusal: the refusal is NOT a
    // 5xx/EOF parser choke, so the #3500 predicate must leave it alone.
    assert!(!is_native_tool_channel_failure(&thrown(
        "sambanova HTTP 400 Bad Request [invalid_request]: Model started a \
             function call but did not complete it."
    )));
}

#[test]
fn billed_noncommittal_throw_ignores_stall_and_unrelated_errors() {
    // The bare provider stall (`delivered no content`) has no tool-channel
    // fingerprint: a same-channel retry is the right move, so the degrade
    // predicate must NOT match it (otherwise a transient hiccup would burn
    // the route's one-shot channel degrade).
    assert!(!is_billed_noncommittal_throw(&thrown(
        "openai-compatible model m reported completion_tokens=12 but delivered \
             no content, reasoning, or tool calls"
    )));
    // A billed-noncommittal phrase WITHOUT the billed-output marker must not
    // trip (mirrors the `completion_tokens=` guard in the empty-completion
    // predicate).
    assert!(!is_billed_noncommittal_throw(&thrown(
        "upstream contract violation with no dispatchable tool call or answer"
    )));
    // A generic 429 / rate limit is never a vanished tool channel.
    assert!(!is_billed_noncommittal_throw(&thrown(
        "[rate_limited] too many requests"
    )));
    // A 400 about a malformed tool SCHEMA (our request was wrong) is not a
    // function-call refusal — degrading the channel would not fix it.
    assert!(!is_billed_noncommittal_throw(&thrown(
        "bad request: tool schema invalid"
    )));
}

#[test]
fn truncation_does_not_trigger_channel_degrade() {
    // REMEDY-ORDER INVARIANT: continue-on-truncation must sit ABOVE
    // channel-switch. A `length`/`max_tokens` truncation (valid tool name,
    // incomplete args) is a budget problem the loop continues/raises budget
    // on — NOT a broken channel. A channel switch invalidates the whole
    // prefix KV cache, so it must never fire for a deterministic truncation
    // that would just re-truncate. The billed-noncommittal *throw* is only
    // produced when the turn finished cleanly (the response-layer detector
    // excludes `stop_reason == length`), so a truncation can never reach the
    // degrade trigger; assert the predicates agree.
    assert!(!is_billed_noncommittal_throw(&thrown(
        "model m hit completion_tokens=2048 length cap mid tool call (truncated)"
    )));
    assert!(!is_native_tool_channel_failure(&thrown(
        "model m hit completion_tokens=2048 length cap mid tool call (truncated)"
    )));
    // (The structural detector's exclusion of `stop_reason == length` from the
    // zero-token empty path is covered by
    // `zero_token_empty_completion_predicate_edges`.)
}

#[test]
fn degrade_options_to_text_channel_strips_native_tool_payload() {
    let mut opts = crate::llm::api::options::base_opts("ollama");
    opts.model = "qwen3.6-35b-a3b".to_string();
    opts.native_tools = Some(vec![serde_json::json!({"name": "edit"})]);
    opts.output_format = crate::llm::api::OutputFormat::JsonObject;
    opts.response_format = Some("json".to_string());
    opts.json_schema = Some(serde_json::json!({"type": "object"}));

    let degraded = degrade_options_to_text_channel(&opts);

    assert!(
        degraded.native_tools.is_none(),
        "the provider-native tool payload must be dropped"
    );
    assert!(
        matches!(degraded.output_format, crate::llm::api::OutputFormat::Text),
        "output must fall back to plain Text so the model answers in content"
    );
    assert!(degraded.response_format.is_none());
    assert!(degraded.json_schema.is_none());
    // The logical request is otherwise unchanged.
    assert_eq!(degraded.provider, "ollama");
    assert_eq!(degraded.model, "qwen3.6-35b-a3b");
    // The original is untouched (we degrade a clone).
    assert!(opts.native_tools.is_some());
}

#[test]
fn template_render_event_round_trips_through_jsonl() {
    use crate::stdlib::template::{
        render_template_to_string, LlmRenderContext, LlmRenderContextGuard,
    };
    let dir = tempfile::tempdir().expect("tempdir");
    push_llm_transcript_dir(dir.path().to_str().expect("utf8"));
    {
        let _ctx =
            LlmRenderContextGuard::enter(LlmRenderContext::resolve("anthropic", "claude-opus-4-7"));
        let rendered = render_template_to_string(
            "{{ if llm.capabilities.native_tools }}native{{ else }}text{{ end }}\
                 {{ section \"task\" }}b{{ endsection }}",
            None,
            None,
            None,
        )
        .expect("render");
        assert!(rendered.contains("native"));
        assert!(rendered.contains("<task>"));
    }
    pop_llm_transcript_dir();
    let transcript =
        std::fs::read_to_string(dir.path().join("llm_transcript.jsonl")).expect("read transcript");
    let line = transcript
        .lines()
        .find(|line| line.contains("\"template.render\""))
        .expect("template.render event present");
    let event: serde_json::Value = serde_json::from_str(line).expect("parse event");
    assert_eq!(event["type"], "template.render");
    assert_eq!(event["llm"]["provider"], "anthropic");
    assert_eq!(event["llm"]["family"], "anthropic-claude");
    assert_eq!(event["llm"]["capabilities"]["native_tools"], true);
    let branches = event["branches"].as_array().expect("branches array");
    let if_branch = branches
        .iter()
        .find(|b| b["kind"] == "if")
        .expect("if branch present");
    assert_eq!(if_branch["branch_id"], "if");
    let section_branch = branches
        .iter()
        .find(|b| b["kind"] == "section")
        .expect("section branch present");
    assert_eq!(section_branch["branch_id"], "xml");
    assert_eq!(section_branch["branch_label"], "task");
}

// Fix B regression: for text-format local models (llamacpp/qwen3.6) the
// tool calls live only inline as `<tool_call>...</tool_call>` in the
// assistant content — the provider-native `result.tool_calls` array is
// EMPTY. The `provider_call_response` observability record used to carry
// only that native array, so the JSONL transcript was not self-describing
// for text-format runs. The record now also carries a `parsed_tool_calls`
// sidecar holding the merged (native OR text-parsed) view.
//
// Critically this is OBSERVABILITY ONLY: the request-construction / history
// path keys off `native_tool_calls` (the native-only list from
// `vm_build_llm_result`, consumed by
// `agent_session_host::assistant_message_from_llm_result`), which the test
// also asserts stays empty for a text-format result — so the model's
// next-turn payload is unchanged.
#[test]
fn response_record_exposes_text_parsed_calls_without_touching_history() {
    use super::super::api::{vm_build_llm_result, LlmResult, ProviderTelemetry};
    use crate::event_log::{
        install_active_event_log, reset_active_event_log, AnyEventLog, EventLog, SqliteEventLog,
        Topic,
    };
    use crate::value::VmValue;
    use std::sync::Arc;

    // Minimal tool registry so the tagged parser resolves the `run` name
    // (mirrors the `tools` registry the request used).
    fn run_tool_registry() -> VmValue {
        let dict = |pairs: &[(&str, VmValue)]| -> VmValue {
            VmValue::dict(
                pairs
                    .iter()
                    .map(|(k, v)| (crate::value::intern_key(k), v.clone()))
                    .collect::<crate::value::DictMap>(),
            )
        };
        let s = |v: &str| VmValue::String(arcstr::ArcStr::from(v));
        let run_tool = dict(&[
            ("name", s("run")),
            ("description", s("Run a shell command.")),
            (
                "parameters",
                dict(&[(
                    "command",
                    dict(&[("type", s("string")), ("description", s("Shell command."))]),
                )]),
            ),
        ]);
        dict(&[("tools", VmValue::List(std::sync::Arc::new(vec![run_tool])))])
    }

    // A text-format completion: native tool_calls EMPTY, the call lives
    // inline as a canonical `<tool_call>` block (already canonicalized from
    // any `[[CALL]]` wire form by the time the result reaches here).
    let text = "<tool_call>\nrun({ command: \"ls\" })\n</tool_call>\n\
## LOOP_STATE\n\
phase: execute\n\
next_phase: verify\n\
done: false\n\
confidence: 0.75\n\
summary: Listed the workspace\n\
## END_LOOP_STATE";
    let result = LlmResult {
        served_fast: false,
        text: text.to_string(),
        tool_calls: Vec::new(),
        raw_tool_calls: Vec::new(),
        input_tokens: 12,
        output_tokens: 5,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cache_supported: true,
        model: "qwen3.6".to_string(),
        provider: "llamacpp".to_string(),
        thinking: None,
        thinking_summary: None,
        stop_reason: Some("stop".to_string()),
        blocks: Vec::new(),
        logprobs: Vec::new(),
        telemetry: ProviderTelemetry::default(),
    };
    let tools = run_tool_registry();

    // 1. Observability path: the response record now exposes the parsed
    //    call via the new sidecar, while native `tool_calls` stays empty.
    let dir = tempfile::tempdir().expect("tempdir");
    reset_active_event_log();
    let event_log = Arc::new(AnyEventLog::Sqlite(
        SqliteEventLog::open(dir.path().join("events.sqlite"), 16).expect("sqlite event log"),
    ));
    install_active_event_log(event_log.clone());
    push_llm_transcript_dir(dir.path().to_str().expect("utf8"));
    dump_llm_response(0, "call-textfmt", &result, 42, None, Some(&tools));
    pop_llm_transcript_dir();

    let transcript =
        std::fs::read_to_string(dir.path().join("llm_transcript.jsonl")).expect("read transcript");
    let line = transcript
        .lines()
        .find(|line| line.contains("\"provider_call_response\""))
        .expect("provider_call_response event present");
    let event: serde_json::Value = serde_json::from_str(line).expect("parse event");

    let native = event["tool_calls"].as_array().expect("tool_calls array");
    assert!(
        native.is_empty(),
        "native tool_calls must remain empty for a text-format result, got: {native:?}"
    );
    let parsed = event["parsed_tool_calls"]
        .as_array()
        .expect("parsed_tool_calls array");
    assert_eq!(
        parsed.len(),
        1,
        "the text-parsed call must surface in the sidecar, got: {parsed:?}"
    );
    assert_eq!(parsed[0]["name"], "run");
    assert_eq!(
        event["loop_state"],
        serde_json::json!({
            "phase": "execute",
            "next_phase": "verify",
            "done": false,
            "confidence": 0.75,
            "summary": "Listed the workspace",
        })
    );
    // The provider-reported stop reason must ride the observability record
    // (an IDE host bug report: it was dropped here, blinding transcript mining to
    // length truncations on every provider route).
    assert_eq!(event["stop_reason"], "stop");

    let topic = Topic::new("agent.transcript.llm").expect("static topic");
    let persisted = futures::executor::block_on(event_log.read_range(&topic, None, 8))
        .expect("read events.sqlite");
    assert_eq!(persisted.len(), 1);
    assert_eq!(
        persisted[0].1.payload["parsed_tool_calls"],
        event["parsed_tool_calls"]
    );
    assert_eq!(persisted[0].1.payload["loop_state"], event["loop_state"]);
    reset_active_event_log();

    // 2. Request-construction / history path is UNCHANGED: the value that
    //    feeds the assistant history envelope is `native_tool_calls`, which
    //    stays empty (native-only) for a text-format result. The merged
    //    `tool_calls` carries the call for unified-view callers, but the
    //    history-feeding native list does not.
    let vm_result = vm_build_llm_result(
        &result,
        None,
        None,
        &crate::llm::api::test_text_projection(&result, Some(&tools)),
    );
    let VmValue::Dict(ref dict) = vm_result else {
        panic!("vm_build_llm_result must return a dict");
    };
    let native_history = dict
        .get("native_tool_calls")
        .expect("native_tool_calls present");
    match native_history {
        VmValue::List(items) => assert!(
            items.is_empty(),
            "native_tool_calls (history-feeding list) must stay empty for a \
                 text-format result, got: {items:?}"
        ),
        other => panic!("native_tool_calls must be a list, got {other:?}"),
    }
    // The merged `tool_calls` (unified view) does carry the call — proving
    // the sidecar mirrors the same merge the result builder already does,
    // not a divergent computation.
    let merged_history = dict.get("tool_calls").expect("tool_calls present");
    match merged_history {
        VmValue::List(items) => assert_eq!(
            items.len(),
            1,
            "merged tool_calls (unified view) should carry the text-parsed call"
        ),
        other => panic!("tool_calls must be a list, got {other:?}"),
    }
}

#[test]
fn loop_state_decoder_requires_a_complete_block_and_preserves_colons() {
    assert_eq!(decode_loop_state("## LOOP_STATE\nphase: execute"), None);
    assert_eq!(
        decode_loop_state(
            "before\n## LOOP_STATE\nsummary: retry: with narrower scope\ndone: true\n## END_LOOP_STATE\nafter",
        ),
        Some(serde_json::json!({
            "summary": "retry: with narrower scope",
            "done": true,
        }))
    );
}

#[test]
fn transcript_dir_option_overrides_env_until_popped() {
    push_llm_transcript_dir("/tmp/harn-transcript-a");
    assert_eq!(
        current_transcript_dir().as_deref(),
        Some("/tmp/harn-transcript-a")
    );
    push_llm_transcript_dir("/tmp/harn-transcript-b");
    assert_eq!(
        current_transcript_dir().as_deref(),
        Some("/tmp/harn-transcript-b")
    );
    pop_llm_transcript_dir();
    assert_eq!(
        current_transcript_dir().as_deref(),
        Some("/tmp/harn-transcript-a")
    );
    pop_llm_transcript_dir();
}

#[test]
fn raw_provider_capture_is_disabled_by_default() {
    let _guard = crate::llm::env_guard();
    let previous_raw = set_env_for_test("HARN_LLM_TRANSCRIPT_RAW", None);
    let dir = temp_transcript_dir("harn-raw-provider-disabled");
    let dir_string = dir.to_string_lossy().to_string();
    push_llm_transcript_dir(&dir_string);

    let context = RawProviderCaptureContext::new("call-disabled".to_string(), 2);
    let path = persist_raw_provider_request(
        Some(&context),
        "openai",
        "gpt-test",
        "openai",
        None,
        &serde_json::json!({"messages": []}),
    );

    pop_llm_transcript_dir();
    restore_env_for_test("HARN_LLM_TRANSCRIPT_RAW", previous_raw);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(path.is_none());
}

#[test]
fn raw_provider_capture_writes_sidecars_and_pointer_events() {
    let _guard = crate::llm::env_guard();
    let previous_raw = set_env_for_test("HARN_LLM_TRANSCRIPT_RAW", Some("1"));
    let dir = temp_transcript_dir("harn-raw-provider-enabled");
    let dir_string = dir.to_string_lossy().to_string();
    push_llm_transcript_dir(&dir_string);

    let context = RawProviderCaptureContext::new("call/raw 1".to_string(), 7);
    let request_path = persist_raw_provider_request(
        Some(&context),
        "openai",
        "gpt-test",
        "openai",
        Some(3),
        &serde_json::json!({"messages": [{"role": "user", "content": "hello"}]}),
    )
    .expect("request sidecar path");
    let response_path = persist_raw_provider_response(
        Some(&context),
        "openai",
        "gpt-test",
        "json",
        Some(3),
        200,
        Some("application/json"),
        r#"{"choices":[{"message":{"content":"done"}}]}"#,
    )
    .expect("response sidecar path");

    pop_llm_transcript_dir();
    restore_env_for_test("HARN_LLM_TRANSCRIPT_RAW", previous_raw);

    assert!(request_path.starts_with("raw-provider/"));
    assert!(request_path.ends_with("-attempt-3-request.json"));
    assert!(response_path.ends_with("-attempt-3-response-json.json"));

    let request_text = std::fs::read_to_string(dir.join(&request_path)).expect("request sidecar");
    let request_json: serde_json::Value =
        serde_json::from_str(&request_text).expect("request json");
    assert_eq!(
        request_json["schema_version"],
        serde_json::json!("harn.llm.raw_provider_request.v1")
    );
    assert_eq!(request_json["wire_dialect"], serde_json::json!("openai"));
    assert_eq!(request_json["attempt"], serde_json::json!(3));

    let response_text =
        std::fs::read_to_string(dir.join(&response_path)).expect("response sidecar");
    let response_json: serde_json::Value =
        serde_json::from_str(&response_text).expect("response json");
    assert_eq!(
        response_json["schema_version"],
        serde_json::json!("harn.llm.raw_provider_response.v1")
    );
    assert_eq!(response_json["transport"], serde_json::json!("json"));
    assert_eq!(response_json["status"], serde_json::json!(200));
    assert!(response_json["body_json"].is_object());

    let transcript = std::fs::read_to_string(dir.join("llm_transcript.jsonl")).expect("transcript");
    assert!(transcript.contains("\"type\":\"provider_raw_capture\""));
    assert!(transcript.contains(&request_path));
    assert!(transcript.contains(&response_path));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "current_thread")]
async fn raw_provider_capture_context_scopes_to_current_task() {
    let context = RawProviderCaptureContext::new("call-context".to_string(), 4);
    with_raw_provider_capture_context(context.clone(), async {
        assert_eq!(current_raw_provider_capture_context(), Some(context));
    })
    .await;

    assert_eq!(current_raw_provider_capture_context(), None);
}

// Regression for #2660. `append_llm_transcript_event_log` used to
// `handle.spawn` the event-log append as a detached task. The agent loop
// and the test runner drive their tokio runtime with
// `LocalSet::run_until`, which stops polling the moment the driving future
// resolves — so those detached appends were never run to completion. Each
// stranded task pinned a transcript-sized payload plus an
// `Arc<AnyEventLog>` clone for the lifetime of the runtime, leaking ~one
// transcript per test across a `harn test --parallel` worker until CI
// OOM'd. The append must therefore complete synchronously: the entry has
// to be readable from the log the instant the producing future resolves,
// without any further runtime polling.
#[test]
fn transcript_event_is_appended_synchronously_under_run_until() {
    use crate::event_log::{
        install_memory_for_current_thread, reset_active_event_log, EventLog, Topic,
    };

    reset_active_event_log();
    let log = install_memory_for_current_thread(128);
    let topic = Topic::new("agent.transcript.llm").expect("static topic");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let local = tokio::task::LocalSet::new();
    runtime.block_on(local.run_until(async {
        // Emit a transcript entry from inside the run_until future, exactly
        // as the agent loop does. With the old detached-spawn path the
        // append task is left scheduled-but-unpolled when this future
        // resolves; with the synchronous path it has already landed.
        append_llm_transcript_entry(&serde_json::json!({
            "type": "provider_call_request",
            "iteration": 0,
            "marker": "regression-2660",
        }));
    }));

    // The driving future has resolved and we are no longer polling the
    // runtime. The event must already be in the log — proving the append
    // ran synchronously rather than on a stranded detached task.
    let latest = futures::executor::block_on(log.latest(&topic))
        .expect("latest query")
        .expect("transcript event must be present immediately after run_until resolves");
    assert_eq!(latest, 1, "exactly one transcript event should be recorded");

    reset_active_event_log();
}

#[test]
fn dump_llm_request_emits_context_breakdown_typed_checkpoint_for_agent_dispatch() {
    use crate::agent_events::{register_sink, reset_all_sinks, AgentEvent, AgentEventSink};
    use std::sync::{Arc, Mutex};

    struct CapturingSink(Arc<Mutex<Vec<AgentEvent>>>);

    impl AgentEventSink for CapturingSink {
        fn handle_event(&self, event: &AgentEvent) {
            self.0
                .lock()
                .expect("captured sink mutex poisoned")
                .push(event.clone());
        }
    }

    reset_all_sinks();
    let session_id = "context-breakdown-session";
    let captured = Arc::new(Mutex::new(Vec::new()));
    register_sink(session_id, Arc::new(CapturingSink(captured.clone())));

    let mut opts = crate::llm::api::options::base_opts("openai");
    opts.session_id = Some(session_id.to_string());
    opts.model = "gpt-4o-mini".to_string();
    opts.system = Some("System policy".to_string());
    opts.messages = vec![serde_json::json!({"role": "user", "content": "fix the bug"})];
    opts.max_tokens = 64;

    dump_llm_request(3, "call-context-1", "json", &opts);

    assert!(
        captured
            .lock()
            .expect("captured sink mutex poisoned")
            .is_empty(),
        "raw/session-scoped llm_call users should not receive agent-loop context checkpoints"
    );

    opts.dispatch_provenance = Some(crate::llm::resolved_dispatch::DispatchProvenance {
        provider: Some(crate::llm::resolved_dispatch::DispatchProvenance::OPERATOR_PIN.to_string()),
        model: Some(crate::llm::resolved_dispatch::DispatchProvenance::OPERATOR_PIN.to_string()),
        wire_format: Some(
            crate::llm::resolved_dispatch::DispatchProvenance::CATALOG_DEFAULT.to_string(),
        ),
        thinking: Some(
            crate::llm::resolved_dispatch::DispatchProvenance::CATALOG_DEFAULT.to_string(),
        ),
        tool_format: Some(
            crate::llm::resolved_dispatch::DispatchProvenance::CATALOG_DEFAULT.to_string(),
        ),
    });

    dump_llm_request(3, "call-context-2", "json", &opts);

    let events = captured.lock().expect("captured sink mutex poisoned");
    let checkpoint = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::TypedCheckpoint {
                session_id: id,
                checkpoint,
            } if id == session_id => Some(checkpoint),
            _ => None,
        })
        .expect("context breakdown typed checkpoint");

    assert_eq!(
        checkpoint["schema"],
        serde_json::json!("harn.llm.context_token_breakdown.v1")
    );
    assert_eq!(checkpoint["call_id"], serde_json::json!("call-context-2"));
    assert_eq!(checkpoint["iteration"], serde_json::json!(3));
    assert_eq!(checkpoint["provider"], serde_json::json!("openai"));
    assert_eq!(checkpoint["model"], serde_json::json!("gpt-4o-mini"));
    assert_eq!(checkpoint["tool_format"], serde_json::json!("json"));
    assert!(
        checkpoint["segments"]
            .as_array()
            .is_some_and(|segments| !segments.is_empty()),
        "typed checkpoint should carry per-segment token accounting"
    );
    assert!(
        checkpoint["context_tokens"].as_i64().unwrap_or_default() > 0,
        "typed checkpoint should carry the projected request total"
    );

    drop(events);
    reset_all_sinks();
}

#[test]
fn mock_provider_retry_backoff_is_zero() {
    assert_eq!(llm_retry_backoff_ms(&thrown("HTTP 503"), 1, "mock"), 0);
}

#[test]
fn equal_jitter_stays_within_ceil_half_to_ceil_bounds() {
    use rand::{rngs::StdRng, SeedableRng};
    // Sweep many seeds across the exponential ceil ladder and assert every
    // draw lands in `[ceil/2, ceil]` — never near-zero (the win over full
    // jitter), never above the exponential ceiling.
    for attempt in 0..8usize {
        let ceil = 250u64.saturating_mul(1 << attempt.min(4));
        let half = ceil / 2;
        for seed in 0..256u64 {
            let mut rng = StdRng::seed_from_u64(seed);
            let wait = equal_jitter_ms(ceil, &mut rng);
            assert!(
                wait >= half && wait <= ceil,
                "attempt={attempt} ceil={ceil} seed={seed} wait={wait} out of [{half}, {ceil}]"
            );
        }
    }
}

#[test]
fn equal_jitter_desynchronizes_concurrent_callers() {
    use rand::{rngs::StdRng, SeedableRng};
    // Two callers drawing from distinct entropy must not resume in lockstep:
    // at least one differing pair proves the herd is broken (the old
    // zero-jitter path returned the identical `ceil` for every caller).
    let ceil = 4000u64;
    let mut any_differ = false;
    for seed in 0..64u64 {
        let a = equal_jitter_ms(ceil, &mut StdRng::seed_from_u64(seed));
        let b = equal_jitter_ms(ceil, &mut StdRng::seed_from_u64(seed + 10_000));
        if a != b {
            any_differ = true;
            break;
        }
    }
    assert!(any_differ, "equal jitter failed to desynchronize any pair");
}

#[test]
fn equal_jitter_tiny_ceil_floors_at_ceil() {
    use rand::{rngs::StdRng, SeedableRng};
    // ceil/2 == 0 (e.g. ceil < 2) cannot host a range; return the ceil.
    let mut rng = StdRng::seed_from_u64(1);
    assert_eq!(equal_jitter_ms(0, &mut rng), 0);
    assert_eq!(equal_jitter_ms(1, &mut rng), 1);
}

#[test]
fn retry_after_path_adds_bounded_jitter() {
    // A provider Retry-After is honored as a floor; the layered jitter never
    // exceeds `retry_after + backoff_base`, so the wait stays in a tight band
    // around the requested cooldown while desynchronizing siblings.
    let err = thrown("HTTP 429 rate_limited retry-after: 5");
    let base = extract_retry_after_ms(&err).expect("retry-after parsed");
    for _ in 0..256 {
        let wait = llm_retry_backoff_ms(&err, 1, "openai");
        assert!(
            wait >= base && wait <= base + DEFAULT_LLM_CALL_BACKOFF_MS,
            "retry-after jitter {wait} out of [{base}, {}]",
            base + DEFAULT_LLM_CALL_BACKOFF_MS
        );
    }
}

#[test]
fn base_backoff_real_provider_respects_exponential_ceiling() {
    // The live (un-seamed) path must still land in the equal-jitter band for
    // the built-in base, exercising `rand::rng()` rather than the test RNG.
    for attempt in 1..=6usize {
        let ceil = DEFAULT_LLM_CALL_BACKOFF_MS.saturating_mul(1 << attempt.min(4));
        for _ in 0..64 {
            let wait = base_retry_backoff_ms(attempt);
            assert!(
                wait >= ceil / 2 && wait <= ceil,
                "attempt={attempt} wait={wait} out of [{}, {ceil}]",
                ceil / 2
            );
        }
    }
}

#[test]
fn categorized_overloaded_is_retryable() {
    assert!(is_retryable_llm_error(&categorized(
        "upstream overloaded",
        ErrorCategory::Overloaded
    )));
}

#[test]
fn overloaded_errors_feed_breaker_but_network_and_server_classes_stay_distinct() {
    // 529 / overloaded_error (the Anthropic overload shapes) must count as
    // breaker-feeding overload...
    assert!(is_overloaded_llm_error(&thrown(
            "anthropic HTTP 529 [http_error]: {\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}"
        )));
    assert!(is_overloaded_llm_error(&categorized(
        "upstream overloaded",
        ErrorCategory::Overloaded
    )));
    // ...while 429 stays rate limiting and generic 500/502 stays a plain
    // server error — neither may trip the breaker.
    assert!(!is_overloaded_llm_error(&thrown(
        "openai HTTP 429 [rate_limited]: too many requests"
    )));
    assert!(!is_overloaded_llm_error(&thrown(
        "openai HTTP 500 [http_error]: internal"
    )));
    assert!(!is_overloaded_llm_error(&categorized(
        "500 internal",
        ErrorCategory::ServerError
    )));
    // Overload is not a *network* failure — it reaches the breaker through
    // its own predicate, not by widening the network classifier.
    assert!(!is_network_failure_llm_error(&categorized(
        "upstream overloaded",
        ErrorCategory::Overloaded
    )));
}

#[test]
fn shared_cooldown_covers_overload_with_default_and_honors_retry_after() {
    // Overload without Retry-After: fixed default window so sibling agents
    // stop hammering the provider.
    assert_eq!(
        shared_cooldown_ms_for_llm_error(&thrown(
            "anthropic HTTP 529 [http_error]: {\"type\":\"overloaded_error\"}"
        )),
        crate::llm::rate_limit::OVERLOAD_COOLDOWN_MS
    );
    // Overload WITH Retry-After: the provider's signal wins.
    assert_eq!(
        shared_cooldown_ms_for_llm_error(&thrown(
            "anthropic HTTP 529 [http_error]: overloaded_error (retry-after: 7)"
        )),
        7_000
    );
    // 429 keeps its existing semantics: Retry-After when sent, no default.
    assert_eq!(
        shared_cooldown_ms_for_llm_error(&thrown(
            "openai HTTP 429 [rate_limited]: slow down (retry-after: 3)"
        )),
        3_000
    );
    assert_eq!(
        shared_cooldown_ms_for_llm_error(&thrown("openai HTTP 429 [rate_limited]: slow down")),
        0
    );
    // Generic 500 and network failures never cool the shared route down.
    assert_eq!(
        shared_cooldown_ms_for_llm_error(&thrown("openai HTTP 500 [http_error]: internal")),
        0
    );
    assert_eq!(
        shared_cooldown_ms_for_llm_error(&categorized(
            "connection reset",
            ErrorCategory::TransientNetwork
        )),
        0
    );
}

#[test]
fn categorized_server_error_is_retryable() {
    assert!(is_retryable_llm_error(&categorized(
        "500 internal",
        ErrorCategory::ServerError
    )));
}

#[test]
fn deterministic_invalid_response_500_is_not_retryable() {
    assert!(!is_retryable_llm_error(&thrown(
        "llamacpp HTTP 500 Internal Server Error [invalid_response]: \
         The model produced output that does not match the expected peg-native format"
    )));
    assert!(is_retryable_llm_error(&thrown(
        "anthropic HTTP 500 Internal Server Error [http_error]: \
         {\"type\":\"overloaded_error\",\"message\":\"Service unavailable\"}"
    )));
}

#[test]
fn categorized_transient_network_is_retryable() {
    assert!(is_retryable_llm_error(&categorized(
        "reset",
        ErrorCategory::TransientNetwork
    )));
}

#[test]
fn categorized_auth_not_retryable() {
    assert!(!is_retryable_llm_error(&categorized(
        "invalid key",
        ErrorCategory::Auth
    )));
}

/// The billed-noncommittal thrown error (`response.rs`/`transport.rs`
/// `billed_noncommittal_completion_error`) is NOT matched by the generic
/// terminal-error classifier — that is precisely why the loop used to
/// hard-break on it — but IS matched by the empty-completion retry
/// predicate so it routes onto the bounded empty-completion budget.
#[test]
fn billed_noncommittal_is_empty_completion_retry_not_generic_retryable() {
    let billed = thrown(
        "provider openrouter model qwen/qwen3.6-35b-a3b returned billed output \
             (completion_tokens=342) with no dispatchable tool call or answer \
             (upstream contract violation): the model finished cleanly but committed \
             neither a tool call nor visible text.",
    );
    assert!(
        !is_retryable_llm_error(&billed),
        "generic terminal classifier must NOT match (root cause of the hard-break)"
    );
    assert!(
        is_empty_completion_retry_error(&billed),
        "must route onto the empty-completion retry budget"
    );
}

/// Edge truth-table for `is_empty_completion_retry_error`: both thrown
/// unproductive shapes match; unrelated errors and partial signatures do
/// not (so a real terminal error never gets laundered into a retry).
#[test]
fn empty_completion_retry_error_edges() {
    // (1) zero-token empty completion (transport guard).
    assert!(is_empty_completion_retry_error(&thrown(
            "openai-compatible model m reported completion_tokens=12 but delivered no content, reasoning, or tool calls"
        )));
    // (2) billed-noncommittal contract violation.
    assert!(is_empty_completion_retry_error(&thrown(
            "provider p model m returned billed output (completion_tokens=5) with no dispatchable tool call or answer (upstream contract violation)"
        )));
    // Also matches when re-wrapped as a categorized error.
    assert!(is_empty_completion_retry_error(&categorized(
            "model m returned billed output (completion_tokens=5) with no dispatchable tool call or answer (upstream contract violation)",
            ErrorCategory::Generic,
        )));
    // No `completion_tokens=` token: not the billed shape.
    assert!(!is_empty_completion_retry_error(&thrown(
        "upstream contract violation with no dispatchable tool call or answer"
    )));
    // A genuine terminal/context error must never match.
    assert!(!is_empty_completion_retry_error(&thrown(
        "local HTTP 400 Bad Request [context_overflow]: prompt is too long"
    )));
    // A 429 is retryable elsewhere but is not an empty-completion shape.
    assert!(!is_empty_completion_retry_error(&thrown(
        "429 too many requests"
    )));
}

#[test]
fn llm_error_kind_dict_gates_retry() {
    let transient = VmError::Thrown(VmValue::dict(std::collections::BTreeMap::from([
        (
            "kind".to_string(),
            VmValue::String(arcstr::ArcStr::from("transient")),
        ),
        (
            "reason".to_string(),
            VmValue::String(arcstr::ArcStr::from("network_error")),
        ),
    ])));
    assert!(is_retryable_llm_error(&transient));

    let terminal = VmError::Thrown(VmValue::dict(std::collections::BTreeMap::from([
        (
            "kind".to_string(),
            VmValue::String(arcstr::ArcStr::from("terminal")),
        ),
        (
            "reason".to_string(),
            VmValue::String(arcstr::ArcStr::from("context_overflow")),
        ),
    ])));
    assert!(!is_retryable_llm_error(&terminal));
}

#[test]
fn context_overflow_message_is_not_retryable() {
    assert!(!is_retryable_llm_error(&thrown(
        "local HTTP 400 Bad Request [context_overflow]: prompt is too long"
    )));
}

#[test]
fn http_503_is_retryable_via_classifier() {
    assert!(is_retryable_llm_error(&thrown(
        "HTTP 503 Service Unavailable"
    )));
}

#[test]
fn http_504_is_retryable() {
    assert!(is_retryable_llm_error(&thrown("HTTP 504 Gateway Timeout")));
}

#[test]
fn http_529_is_retryable() {
    assert!(is_retryable_llm_error(&thrown("HTTP 529 overloaded_error")));
}

#[test]
fn bad_gateway_string_is_retryable() {
    assert!(is_retryable_llm_error(&thrown("bad gateway response")));
}

#[test]
fn service_unavailable_string_is_retryable() {
    assert!(is_retryable_llm_error(&thrown("service unavailable")));
}

#[test]
fn auth_error_not_retryable() {
    assert!(!is_retryable_llm_error(&thrown("HTTP 401 Unauthorized")));
}

#[test]
fn retry_after_integer_seconds() {
    assert_eq!(parse_retry_after("err: retry-after: 5"), Some(5_000));
}

#[test]
fn retry_after_fractional_seconds() {
    assert_eq!(parse_retry_after("retry-after: 2.5"), Some(2_500));
}

#[test]
fn retry_after_seconds_with_provider_message_punctuation() {
    let msg = "cerebras HTTP 429 Too Many Requests [rate_limited]: Tokens per minute limit exceeded (type: too_many_tokens_error, code: token_quota_exceeded) (retry-after: 60))";
    assert_eq!(parse_retry_after(msg), Some(60_000));
}

#[test]
fn retry_after_clamped_to_cap() {
    assert_eq!(parse_retry_after("retry-after: 600"), Some(60_000));
}

#[test]
fn retry_after_http_date_past_is_zero() {
    let past = "retry-after: Mon, 01 Jan 1990 00:00:00 GMT";
    assert_eq!(parse_retry_after(past), Some(0));
}

#[test]
fn retry_after_missing_returns_none() {
    assert_eq!(parse_retry_after("nothing here"), None);
}

#[test]
fn retry_after_malformed_returns_none() {
    assert_eq!(parse_retry_after("retry-after: soon-ish"), None);
}
