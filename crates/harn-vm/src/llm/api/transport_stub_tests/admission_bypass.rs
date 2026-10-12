use super::*;
use crate::llm::api::{LlmCallOptions, LlmResult};
use crate::llm::rate_limit::{provider_request_usage, set_rate_limits};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};

const PROVIDER: &str = "admission-owner";

struct Cleanup;
impl Drop for Cleanup {
    fn drop(&mut self) {
        crate::reset_thread_local_state();
        crate::llm_config::clear_user_overrides();
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn options(provider: &str) -> LlmCallOptions {
    LlmCallOptions {
        provider: provider.into(),
        model: "admission-model".into(),
        max_tokens: 16,
        stream: false,
        messages: vec![serde_json::json!({"role":"user", "content":"quota proof"})],
        ..Default::default()
    }
}

fn limits(provider: &str) {
    set_rate_limits(
        provider,
        crate::llm_config::RateLimitsDef {
            rpm: Some(1000),
            concurrency: Some(1),
            ..Default::default()
        },
    );
}

async fn call(opts: &LlmCallOptions, offthread: bool) -> (LlmResult, Vec<String>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let result = if offthread {
        vm_call_llm_full_streaming_offthread(opts, tx).await
    } else {
        vm_call_llm_full_streaming(opts, tx).await
    }
    .unwrap();
    let mut deltas = Vec::new();
    while let Ok(delta) = rx.try_recv() {
        deltas.push(delta);
    }
    (result, deltas)
}

fn chat_stub(
    requests: Arc<AtomicUsize>,
    expected_stream: bool,
    mut first_body_gate: Option<(tokio::sync::oneshot::Sender<()>, mpsc::Receiver<()>)>,
) -> LlmStub {
    spawn_llm_stub_many("physical admission", 2, move |attempt, stream| {
        let (headers, body) = ollama_openai_compat::read_http_request(stream);
        assert!(headers.starts_with("POST /v1/chat/completions "));
        assert_eq!(body["model"], "admission-model");
        if expected_stream {
            assert_eq!(body["stream"], true, "wire streaming mode");
        } else {
            assert!(body.get("stream").is_none(), "nonstream wire omits stream");
        }
        requests.fetch_add(1, Ordering::SeqCst);
        let (content_type, response) = if body["stream"] == true {
            (
                "text/event-stream",
                format!(
                    "data: {}\n\ndata: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    serde_json::json!({"model":"admission-model", "choices":[{"index":0, "delta":{"content":"physical-"}, "finish_reason":null}]}),
                    serde_json::json!({"model":"admission-model", "choices":[{"index":0, "delta":{"content":"reached"}, "finish_reason":null}]}),
                    serde_json::json!({"model":"admission-model", "choices":[{"index":0, "delta":{}, "finish_reason":"stop"}], "usage":{"prompt_tokens":3, "completion_tokens":1}}),
                ),
            )
        } else {
            (
                "application/json",
                serde_json::json!({"id":"admission-proof", "model":"admission-model",
                    "choices":[{"message":{"role":"assistant", "content":"physical-reached"}, "finish_reason":"stop"}],
                    "usage":{"prompt_tokens":3, "completion_tokens":1}}).to_string(),
            )
        };
        write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", response.len()).unwrap();
        stream.flush().unwrap();
        if attempt == 0 {
            if let Some((received, release)) = first_body_gate.take() {
                assert!(received.send(()).is_ok());
                release.recv().unwrap();
            }
        }
        stream.write_all(response.as_bytes()).unwrap();
    })
}

fn assert_cache_or_replay_bypass(offthread: bool, replay: bool) {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _durable = ScopedEnvVar::set("HARN_LLM_RATE_LIMIT_DURABLE", "0");
    crate::reset_thread_local_state();
    let _cleanup = Cleanup;
    let dir = tempfile::tempdir().unwrap();
    runtime().block_on(async {
        let requests = Arc::new(AtomicUsize::new(0));
        let server = chat_stub(requests.clone(), true, None);
        install_openai_stub_provider(PROVIDER, server.addr());
        limits(PROVIDER);
        let mut opts = options(PROVIDER);
        opts.stream = true;
        crate::llm::set_replay_mode(
            crate::llm::LlmReplayMode::Record,
            dir.path().to_str().unwrap(),
        );
        crate::llm::mock::enable_cli_llm_mock_recording();
        let predicate =
            crate::llm::trigger_predicate::start_predicate_evaluation(Default::default(), vec![]);
        let (first, first_deltas) = call(&opts, false).await;
        assert_eq!(first.text, "physical-reached");
        assert_eq!(
            first_deltas.concat(),
            first.text,
            "actual SSE text reached the streaming caller once and in order"
        );
        let recordings = crate::llm::mock::take_cli_llm_recordings();
        assert_eq!(recordings.len(), 1, "real CLI capture was reached");
        assert_eq!(recordings[0].text, first.text);
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(
            provider_request_usage(PROVIDER),
            1,
            "physical quota instrument reached"
        );
        let captured = predicate.finish();
        assert_eq!(captured.entries.len(), 1);
        assert!(!captured.cached);
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "actual recorded response"
        );
        let predicate = if replay {
            crate::llm::set_replay_mode(
                crate::llm::LlmReplayMode::Replay,
                dir.path().to_str().unwrap(),
            );
            None
        } else {
            crate::llm::set_replay_mode(crate::llm::LlmReplayMode::Off, "");
            Some(crate::llm::trigger_predicate::start_predicate_evaluation(
                Default::default(),
                captured.entries,
            ))
        };
        let (local, deltas) = call(&opts, offthread).await;
        assert_eq!(local.text, first.text);
        if let Some(predicate) = predicate {
            assert!(
                predicate.finish().cached,
                "real predicate cache branch fired"
            );
        }
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "bypass did not perform another HTTP request"
        );
        assert_eq!(
            provider_request_usage(PROVIDER),
            1,
            "cache/replay consumes no additional provider slot"
        );
        let recordings = crate::llm::mock::take_cli_llm_recordings();
        assert_eq!(
            (deltas, recordings.len()),
            (vec![first.text.clone()], 1),
            "the completed bypass must emit text once and capture one CLI result"
        );
        assert_eq!(recordings[0].text, first.text);
    });
}

#[test]
fn inline_and_offthread_cache_hits_do_not_consume_provider_quota() {
    for offthread in [false, true] {
        assert_cache_or_replay_bypass(offthread, false);
    }
}

#[test]
fn inline_recorded_replay_preserves_stream_capture_without_provider_quota() {
    assert_cache_or_replay_bypass(false, true);
}

#[test]
fn offthread_recorded_replay_preserves_stream_capture_without_provider_quota() {
    assert_cache_or_replay_bypass(true, true);
}

#[test]
fn inline_and_offthread_mock_and_fake_calls_do_not_consume_provider_quota() {
    let _env = env_guard();
    let _durable = ScopedEnvVar::set("HARN_LLM_RATE_LIMIT_DURABLE", "0");
    crate::reset_thread_local_state();
    let _cleanup = Cleanup;
    runtime().block_on(async {
        for offthread in [false, true] {
            crate::reset_thread_local_state();
            limits(PROVIDER);
            let mock = crate::llm::jsonl::parse_llm_mock_value(
                &serde_json::json!({"stream_chunks":["mock-", "reached"]}),
            )
            .unwrap();
            crate::llm::mock::install_cli_llm_mocks(vec![mock]);
            let (result, deltas) = call(&options(PROVIDER), offthread).await;
            assert_eq!(result.text, "mock-reached");
            assert_eq!(
                deltas,
                ["mock-", "reached"],
                "scripted mock order is preserved"
            );
            assert_eq!(provider_request_usage(PROVIDER), 0);
            crate::llm::mock::clear_cli_llm_mock_mode();
            limits("fake");
            let _fake = crate::llm::fake::install_fake_llm_script(
                crate::llm::fake::FakeLlmScript::streaming([
                    crate::llm::fake::FakeLlmEvent::Token("fake-".into()),
                    crate::llm::fake::FakeLlmEvent::Token("reached".into()),
                    crate::llm::fake::FakeLlmEvent::Done(crate::llm::fake::FakeStopReason::EndTurn),
                ]),
            );
            let (result, deltas) = call(&options("fake"), offthread).await;
            assert_eq!(result.text, "fake-reached");
            assert_eq!(
                deltas,
                ["fake-", "reached"],
                "fake transport is not emitted again"
            );
            assert_eq!(provider_request_usage("fake"), 0);
        }
    });
}

#[test]
fn detached_offthread_transport_keeps_its_permit_until_body_completion() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _durable = ScopedEnvVar::set("HARN_LLM_RATE_LIMIT_DURABLE", "0");
    crate::reset_thread_local_state();
    let _cleanup = Cleanup;
    let dir = tempfile::tempdir().unwrap();
    runtime().block_on(async {
        let requests = Arc::new(AtomicUsize::new(0));
        let (received_tx, received_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = chat_stub(requests.clone(), false, Some((received_tx, release_rx)));
        let mut release = ReleaseBody(Some(release_tx));
        install_openai_stub_provider(PROVIDER, server.addr());
        limits(PROVIDER);
        let opts = options(PROVIDER);
        crate::llm::set_replay_mode(
            crate::llm::LlmReplayMode::Record,
            dir.path().to_str().unwrap(),
        );
        crate::llm::mock::enable_cli_llm_mock_recording();
        let mut detached = Box::pin(call(&opts, true));
        tokio::select! {
            received = received_rx => received.unwrap(),
            _ = &mut detached => panic!("transport finished before its body was released"),
        }
        assert_eq!(provider_request_usage(PROVIDER), 1);
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        drop(detached);
        // The next caller changes its policy; the existing worker retains its capture.
        crate::llm::set_replay_mode(crate::llm::LlmReplayMode::Off, "");
        let mut second_opts = opts.clone();
        second_opts.messages = vec![serde_json::json!({"role":"user", "content":"second request"})];
        let mut second = Box::pin(call(&second_opts, false));
        assert!(futures::poll!(&mut second).is_pending());
        assert_eq!(
            provider_request_usage(PROVIDER),
            1,
            "detached I/O still owns the first permit"
        );
        release.release();
        let (result, _) = second.await;
        assert_eq!(result.text, "physical-reached");
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        assert_eq!(provider_request_usage(PROVIDER), 2);
        let fixtures: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(
            fixtures.len(),
            1,
            "detached worker retained its record destination"
        );
        let fixture: serde_json::Value =
            serde_json::from_slice(&std::fs::read(fixtures[0].as_ref().unwrap().path()).unwrap())
                .unwrap();
        assert_eq!(fixture["text"], "physical-reached");
        assert_eq!(
            crate::llm::mock::take_cli_llm_recordings().len(),
            2,
            "both actual responses were captured"
        );
    });
}
