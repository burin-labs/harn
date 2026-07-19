//! Unit tests for [`super`] — the versioned, scoped LLM mock-fixture engine.
//! Split out of `mock.rs` to keep that file under the source-length ceiling;
//! included via `#[path = "mock_tests.rs"] mod tests;`.

use super::*;
use crate::agent_events::{AgentEvent, AgentEventSink};
use crate::llm::api::LlmRequestPayload;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::task::Poll;

fn text_mock(text: &str) -> LlmMock {
    LlmMock {
        text: text.to_string(),
        tool_calls: Vec::new(),
        raw_tool_calls: Vec::new(),
        match_pattern: None,
        scope: DEFAULT_MOCK_SCOPE.to_string(),
        entry_id: String::new(),
        sticky: false,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        thinking: None,
        thinking_summary: None,
        stop_reason: None,
        model: "fixture-model".to_string(),
        provider: None,
        blocks: None,
        logprobs: Vec::new(),
        error: None,
        stream_chunks: Vec::new(),
    }
}

#[test]
fn build_mock_result_surfaces_fixture_tool_calls() {
    // The CLI-mock fixture shape a downstream native-tool test uses.
    let mock = crate::llm::jsonl::parse_llm_mock_value(&serde_json::json!({
        "match": "*",
        "consume_match": true,
        "tool_calls": [{"name": "ask_user", "arguments": {"question": "Which?"}}]
    }))
    .expect("parse mock");
    assert!(
        !mock.tool_calls.is_empty(),
        "fixture tool_calls must parse into the mock: {:?}",
        mock.tool_calls
    );
    let result = build_mock_result(&mock, 10);
    assert!(
        !result.tool_calls.is_empty(),
        "build_mock_result must surface tool_calls: {:?}",
        result.tool_calls
    );
    assert_eq!(result.tool_calls[0]["name"], "ask_user");
}

#[test]
fn cli_llm_mock_replay_scope_survives_provider_worker_thread() {
    reset_llm_mock_state();
    install_cli_llm_mocks(vec![text_mock("cross-thread replay")]);
    let request = LlmRequestPayload::from(&crate::llm::api::options::base_opts("anthropic"));

    assert!(request.cli_llm_mock_scope.is_some());
    assert!(crate::llm::providers::MockProvider::should_intercept_request(&request));

    let result = std::thread::spawn(move || {
        assert!(crate::llm::providers::MockProvider::should_intercept_request(&request));
        mock_llm_response(&request)
    })
    .join()
    .expect("provider worker thread")
    .expect("mock response");

    assert_eq!(result.text, "cross-thread replay");
    clear_cli_llm_mock_mode();
}

#[test]
fn cli_llm_mock_record_scope_collects_provider_worker_thread_results() {
    reset_llm_mock_state();
    enable_cli_llm_mock_recording();
    let request = LlmRequestPayload::from(&crate::llm::api::options::base_opts("anthropic"));
    let result = build_mock_result(&text_mock("cross-thread record"), 7);

    assert!(request.cli_llm_mock_scope.is_some());
    std::thread::spawn(move || record_cli_llm_result(&request, &result))
        .join()
        .expect("provider worker thread");

    let recordings = take_cli_llm_recordings();
    assert_eq!(recordings.len(), 1);
    assert_eq!(recordings[0].text, "cross-thread record");
    clear_cli_llm_mock_mode();
}

#[test]
fn offthread_mock_call_keeps_the_callers_logical_context() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("runtime");

    runtime.block_on(async {
        reset_llm_mock_state();
        push_llm_mock(text_mock("logical mock"));
        let opts = crate::llm::api::options::base_opts("mock");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let result = crate::llm::api::vm_call_llm_full_streaming_offthread(&opts, tx)
            .await
            .expect("off-thread mock call");

        assert_eq!(result.text, "logical mock");
        assert_eq!(get_llm_mock_calls().len(), 1);
        assert_eq!(get_llm_mock_receipts().len(), 1);
        reset_llm_mock_state();
    });
}

#[test]
fn inline_mock_scope_survives_a_deterministic_executor_thread_hop() {
    reset_llm_mock_state();
    let ambient = crate::orchestration::AmbientExecutionScope::capture_for_top_level_execution(
        crate::observability::execution_scope::mint_execution_scope(),
        LlmMockContext::default(),
    );
    let mut first_poll = true;
    let inner = std::future::poll_fn(move |cx| {
        if first_poll {
            first_poll = false;
            push_llm_mock_scope();
            push_llm_mock(text_mock("thread-hop"));
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }

        let request = LlmRequestPayload::from(&crate::llm::api::options::base_opts("mock"));
        let result = mock_llm_response(&request).expect("mock after thread hop");
        let calls = get_llm_mock_calls().len();
        let receipts = get_llm_mock_receipts().len();
        let popped = pop_llm_mock_scope();
        Poll::Ready((
            result.text,
            calls,
            receipts,
            popped,
            builtin_llm_mock_active(),
        ))
    });
    let mut scoped = Box::pin(crate::orchestration::scope_ambient(ambient, inner));
    let waker = futures::task::noop_waker();
    let mut cx = std::task::Context::from_waker(&waker);
    assert!(matches!(scoped.as_mut().poll(&mut cx), Poll::Pending));

    let observed = std::thread::spawn(move || {
        let waker = futures::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        match scoped.as_mut().poll(&mut cx) {
            Poll::Ready(observed) => observed,
            Poll::Pending => panic!("second poll must complete"),
        }
    })
    .join()
    .expect("executor thread");

    assert_eq!(observed, ("thread-hop".to_string(), 1, 1, true, false));
    assert!(!builtin_llm_mock_active(), "scope must not leak to caller");
}

fn fixture_script(prompt: &str) -> crate::Chunk {
    crate::compile_source(&format!(
        r#"
yield_now()
const response = llm_call("{prompt}", nil, {{provider: "mock"}})
__io_println(response.text)
__io_println(len(llm_mock_calls()))
"#
    ))
    .expect("compile fixture script")
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_vms_isolate_inline_queues_and_call_logs() {
    let alpha = crate::compile_source(
        r#"
llm_mock({text: "alpha"})
yield_now()
const response = llm_call("alpha prompt", nil, {provider: "mock"})
__io_println(response.text)
__io_println(len(llm_mock_calls()))
"#,
    )
    .expect("compile alpha");
    let beta = crate::compile_source(
        r#"
llm_mock({text: "beta"})
yield_now()
const response = llm_call("beta prompt", nil, {provider: "mock"})
__io_println(response.text)
__io_println(len(llm_mock_calls()))
"#,
    )
    .expect("compile beta");
    let mut alpha_vm = crate::Vm::new();
    let mut beta_vm = crate::Vm::new();
    crate::register_vm_stdlib(&mut alpha_vm);
    crate::register_vm_stdlib(&mut beta_vm);

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (alpha_result, beta_result) =
                tokio::join!(alpha_vm.execute(&alpha), beta_vm.execute(&beta));
            alpha_result.expect("alpha VM");
            beta_result.expect("beta VM");
        })
        .await;

    assert_eq!(alpha_vm.output(), "alpha\n1\n");
    assert_eq!(beta_vm.output(), "beta\n1\n");
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_vms_keep_distinct_cli_fixture_leases() {
    reset_llm_mock_state();
    install_cli_llm_mocks(vec![text_mock("cli-alpha")]);
    let mut alpha_vm = crate::Vm::new();
    crate::register_vm_stdlib(&mut alpha_vm);

    install_cli_llm_mocks(vec![text_mock("cli-beta")]);
    let mut beta_vm = crate::Vm::new();
    crate::register_vm_stdlib(&mut beta_vm);
    clear_cli_llm_mock_mode();

    let alpha = fixture_script("alpha fixture");
    let beta = fixture_script("beta fixture");
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (alpha_result, beta_result) =
                tokio::join!(alpha_vm.execute(&alpha), beta_vm.execute(&beta));
            alpha_result.expect("alpha CLI fixture");
            beta_result.expect("beta CLI fixture");
        })
        .await;

    assert_eq!(alpha_vm.output(), "cli-alpha\n1\n");
    assert_eq!(beta_vm.output(), "cli-beta\n1\n");
    drop(alpha_vm);
    drop(beta_vm);
    assert!(cli_llm_mock_scopes().is_empty());
}

#[test]
fn cli_mock_native_tool_calls_reach_the_live_result() {
    // Exercises the full CLI `--llm-mock` path (install scope -> match ->
    // build) the burin native-tool fixture uses, which the isolated
    // parse/build/message tests skip. If this yields an empty result, the
    // downstream native-tool mock test sees zero tool-call events.
    reset_llm_mock_state();
    let mocks = vec![crate::llm::jsonl::parse_llm_mock_value(&serde_json::json!({
        "match": "*",
        "consume_match": true,
        "tool_calls": [{"name": "ask_user", "arguments": {"question": "Which?"}}]
    }))
    .expect("parse mock")];
    install_cli_llm_mocks(mocks);
    let request = LlmRequestPayload::from(&crate::llm::api::options::base_opts("fixture"));
    assert!(
        request.cli_llm_mock_scope.is_some(),
        "cli mock scope must be active"
    );
    let result = mock_llm_response(&request).expect("mock response");
    clear_cli_llm_mock_mode();
    assert!(
        !result.tool_calls.is_empty(),
        "CLI mock native tool_calls must reach the live result: text={:?} tool_calls={:?}",
        result.text,
        result.tool_calls
    );
}

// --- Versioned mock-fixture contract (#4984) ---

/// Build a request that draws from `scope`, carrying `prompt` as the sole
/// user message. Installing the fixture first means the `From` impl captures
/// the live CLI mock scope handle.
fn request_with_scope(prompt: &str, scope: Option<&str>) -> LlmRequestPayload {
    let mut opts = crate::llm::api::options::base_opts("fixture");
    opts.messages = vec![serde_json::json!({"role": "user", "content": prompt})];
    opts.mock_scope = scope.map(str::to_string);
    LlmRequestPayload::from(&opts)
}

struct EventSink(Arc<Mutex<Vec<AgentEvent>>>);

impl AgentEventSink for EventSink {
    fn handle_event(&self, event: &AgentEvent) {
        self.0.lock().expect("event sink lock").push(event.clone());
    }
}

/// Assemble an in-memory v1 fixture for queue-matching tests. Parser tests
/// separately require authored metadata; these tests focus on store behavior.
fn v1_fixture(strict_scopes: bool, entries: &[serde_json::Value]) -> LlmMockFixture {
    let mocks = entries
        .iter()
        .enumerate()
        .map(|(idx, value)| {
            let mut entry = value.as_object().expect("fixture entry object").clone();
            entry
                .entry("id".to_string())
                .or_insert_with(|| serde_json::json!(format!("test-{idx}")));
            entry
                .entry("scope".to_string())
                .or_insert_with(|| serde_json::json!(DEFAULT_MOCK_SCOPE));
            entry
                .entry("consume".to_string())
                .or_insert_with(|| serde_json::json!("once"));
            crate::llm::jsonl::parse_llm_mock_value_versioned(
                &serde_json::Value::Object(entry),
                1,
                idx,
            )
            .expect("parse v1 fixture entry")
        })
        .collect();
    LlmMockFixture {
        schema_version: 1,
        strict_scopes,
        mocks,
        warnings: Vec::new(),
    }
}

#[test]
fn scoped_fixture_serves_main_and_judge_from_their_own_buckets() {
    // With a shared first-match-wins queue this is unwritable — the
    // judge call would cannibalize the main entry. Scoped buckets keep them
    // apart.
    reset_llm_mock_state();
    install_cli_llm_mock_fixture(v1_fixture(
        false,
        &[
            serde_json::json!({"scope": "agent.main", "text": "MAIN"}),
            serde_json::json!({"scope": "completion.judge", "text": "JUDGE"}),
        ],
    ));

    let main = mock_llm_response(&request_with_scope("turn", Some("agent.main"))).expect("main");
    assert_eq!(main.text, "MAIN");
    let judge =
        mock_llm_response(&request_with_scope("verify", Some("completion.judge"))).expect("judge");
    assert_eq!(judge.text, "JUDGE");

    let receipts = get_llm_mock_receipts();
    assert_eq!(receipts.len(), 2);
    assert_eq!(receipts[0].id, "test-0");
    assert_eq!(receipts[0].resolved_scope, "agent.main");
    assert!(receipts[0].matched);
    assert_eq!(receipts[1].resolved_scope, "completion.judge");
    clear_cli_llm_mock_mode();
}

#[test]
fn aux_call_falls_through_to_default_never_to_main() {
    // The core fix: a judge call with no judge entry reaches the shared
    // `default` bucket but must NEVER consume the `main` entry.
    reset_llm_mock_state();
    install_cli_llm_mock_fixture(v1_fixture(
        false,
        &[
            serde_json::json!({"scope": "agent.main", "text": "MAIN"}),
            serde_json::json!({"scope": "default", "text": "DEFAULT"}),
        ],
    ));

    let judge =
        mock_llm_response(&request_with_scope("verify", Some("completion.judge"))).expect("judge");
    assert_eq!(
        judge.text, "DEFAULT",
        "unscoped-aux call must reach default"
    );

    // The main entry is untouched: a real main call still gets it.
    let main = mock_llm_response(&request_with_scope("turn", Some("agent.main"))).expect("main");
    assert_eq!(main.text, "MAIN");

    let receipts = get_llm_mock_receipts();
    assert_eq!(
        receipts[0].resolved_scope, "default",
        "judge drew from default bucket"
    );
    assert!(receipts[0].matched);
    clear_cli_llm_mock_mode();
}

#[test]
fn v0_unscoped_fixture_replays_in_fifo_order() {
    // Back-compat golden: an unscoped v0 fixture keeps first-match-wins FIFO
    // consumption, unchanged by the scope machinery.
    reset_llm_mock_state();
    let mocks = vec![
        crate::llm::jsonl::parse_llm_mock_value(&serde_json::json!({"text": "first"}))
            .expect("parse first"),
        crate::llm::jsonl::parse_llm_mock_value(&serde_json::json!({"text": "second"}))
            .expect("parse second"),
    ];
    install_cli_llm_mocks(mocks);

    assert_eq!(
        mock_llm_response(&request_with_scope("a", None))
            .expect("first")
            .text,
        "first"
    );
    assert_eq!(
        mock_llm_response(&request_with_scope("b", None))
            .expect("second")
            .text,
        "second"
    );
    clear_cli_llm_mock_mode();
}

#[test]
fn sticky_entry_reused_while_once_entry_is_consumed() {
    reset_llm_mock_state();
    install_cli_llm_mock_fixture(v1_fixture(
        false,
        &[
            serde_json::json!({"scope": "completion.judge", "match": "*", "consume": "sticky", "text": "STICKY"}),
            serde_json::json!({"scope": "agent.main", "text": "ONCE"}),
        ],
    ));

    for _ in 0..3 {
        assert_eq!(
            mock_llm_response(&request_with_scope("q", Some("completion.judge")))
                .expect("sticky")
                .text,
            "STICKY"
        );
    }

    assert_eq!(
        mock_llm_response(&request_with_scope("t", Some("agent.main")))
            .expect("once")
            .text,
        "ONCE"
    );
    // The one-shot main entry is gone: a second main call misses (no default
    // bucket to fall to) and, under replay, errors.
    assert!(
        mock_llm_response(&request_with_scope("t2", Some("agent.main"))).is_err(),
        "a consumed once-entry must not replay"
    );
    clear_cli_llm_mock_mode();
}

#[test]
fn strict_scopes_makes_unscoped_aux_a_hard_miss() {
    reset_llm_mock_state();
    install_cli_llm_mock_fixture(v1_fixture(
        true,
        &[serde_json::json!({"scope": "default", "text": "DEFAULT"})],
    ));

    // strictScopes forbids the default fall-through, so a judge call misses.
    assert!(
        mock_llm_response(&request_with_scope("verify", Some("completion.judge"))).is_err(),
        "strict scopes must make an unscoped-aux call a hard miss"
    );
    let receipts = get_llm_mock_receipts();
    assert!(
        receipts
            .iter()
            .any(|r| r.requested_scope == "completion.judge" && !r.matched),
        "the hard miss must be recorded as an unmatched receipt: {receipts:?}"
    );

    // The default entry was never touched — an explicit default call hits it.
    let def = mock_llm_response(&request_with_scope("x", None)).expect("default");
    assert_eq!(def.text, "DEFAULT");
    clear_cli_llm_mock_mode();
}

#[test]
fn matched_receipt_emits_typed_checkpoint() {
    reset_llm_mock_state();
    let events = Arc::new(Mutex::new(Vec::new()));
    let handle = crate::agent_events::register_wildcard_sink(Arc::new(EventSink(events.clone())));
    let mut opts = crate::llm::api::options::base_opts("fixture");
    opts.messages = vec![serde_json::json!({"role": "user", "content": "turn"})];
    opts.session_id = Some("mock-session".to_string());
    opts.mock_scope = Some("agent.main".to_string());
    install_cli_llm_mock_fixture(v1_fixture(
        true,
        &[serde_json::json!({"scope": "agent.main", "text": "MAIN"})],
    ));

    let request = LlmRequestPayload::from(&opts);
    mock_llm_response(&request).expect("scoped response");
    clear_cli_llm_mock_mode();
    crate::agent_events::unregister_wildcard_sink(handle);

    let events = events.lock().expect("event sink lock");
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::TypedCheckpoint {
            session_id,
            checkpoint: receipt,
        }
            if session_id == "mock-session"
                && receipt["schema"] == "harn.llm_mock_fixture_consumption.v1"
                && receipt["resolved_scope"] == "agent.main"
    )));
}
