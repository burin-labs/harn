use super::*;
use crate::llm::api::test_support::{allow_stubbed_llm_transport, ReleaseBody, ScopedEnvVar};
use crate::llm::api::transport_stub_tests::{
    install_openai_stub_provider, read_http_request, spawn_llm_stub, LlmStub,
};
use crate::value::{DictMap, VmDictExt};
use std::path::Path;
use std::sync::{Arc, Mutex};

const SECRET: &str = "Bearer unit-test-provider-capture-secret";

fn response(marker: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "capture-response", "object": "chat.completion", "model": "gpt-4o-mini",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": format!("answer {marker} {SECRET}")}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 2, "completion_tokens": 1, "total_tokens": 3},
    })
}

struct Cleanup;
impl Drop for Cleanup {
    fn drop(&mut self) {
        crate::llm_config::clear_user_overrides();
        crate::llm_config::clear_runtime_provider_endpoint_overrides();
        crate::llm::capabilities::clear_user_overrides();
    }
}

fn stub(
    marker: &str,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
) -> (LlmStub, tokio::sync::oneshot::Receiver<()>, ReleaseBody) {
    let marker = marker.to_string();
    let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let server = spawn_llm_stub("per-call raw capture", move |stream| {
        use std::io::Write;
        let (_, request) = read_http_request(stream);
        requests.lock().unwrap().push(request);
        let _ = seen_tx.send(());
        let _ = release_rx.recv();
        let body = response(&marker).to_string();
        write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    (server, seen_rx, ReleaseBody(Some(release_tx)))
}

fn options(
    provider: &str,
    dir: Option<&Path>,
    raw: Option<bool>,
    marker: &str,
) -> api::LlmCallOptions {
    api::LlmCallOptions {
        provider: provider.into(),
        model: "gpt-4o-mini".into(),
        max_tokens: 64,
        messages: vec![
            serde_json::json!({"role": "user", "content": format!("prompt {marker} {SECRET}")}),
        ],
        llm_transcript_dir: dir.map(|dir| dir.to_str().unwrap().to_string()),
        llm_transcript_raw: raw,
        stream: false,
        ..Default::default()
    }
}

async fn call(opts: &api::LlmCallOptions, offthread: bool) -> Result<api::LlmResult, VmError> {
    observed_llm_call(opts, None, None, None, false, offthread, None, None).await
}

fn transcript(dir: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(dir.join("llm_transcript.jsonl"))
        .expect("selected transcript")
        .lines()
        .map(|line| serde_json::from_str(line).expect("transcript event"))
        .collect()
}

fn assert_capture(dir: &Path, marker: &str, enabled: bool, wire: &serde_json::Value) {
    let events = transcript(dir);
    assert!(events
        .iter()
        .any(|event| event["type"] == "provider_call_request"));
    assert!(events
        .iter()
        .any(|event| event["type"] == "provider_call_response"));
    let pointers: Vec<_> = events
        .iter()
        .filter(|event| event["type"] == "provider_raw_capture")
        .collect();
    assert_eq!(pointers.len(), if enabled { 2 } else { 0 });
    if enabled {
        let mut kinds = std::collections::BTreeSet::new();
        for pointer in pointers {
            let text =
                std::fs::read_to_string(dir.join(pointer["path"].as_str().unwrap())).unwrap();
            assert!(!text.contains(SECRET), "persisted secret escaped redaction");
            let receipt: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(receipt["call_id"], pointer["call_id"]);
            assert!(text.contains(marker), "sidecar belongs to another call");
            let field = if receipt["kind"] == "request" {
                "body"
            } else {
                "body_json"
            };
            let mut expected = if field == "body" {
                wire.clone()
            } else {
                response(marker)
            };
            crate::redact::current_policy().redact_json_in_place(&mut expected);
            assert_eq!(
                receipt[field], expected,
                "captured payload differs from wire"
            );
            kinds.insert(receipt["kind"].as_str().unwrap().to_string());
        }
        assert_eq!(
            kinds,
            std::collections::BTreeSet::from(["request".into(), "response".into()])
        );
    } else {
        assert!(!dir.join("raw-provider").exists());
    }
    assert!(events
        .iter()
        .all(|event| event["type"] != "provider_raw_capture_failed"));
}

#[test]
fn capture_options_are_typed_local_only_and_preserved_by_agent_projection() {
    let _env = crate::llm::env_guard();
    for raw in [false, true] {
        let mut fields = DictMap::new();
        fields.put_str("provider", "mock");
        fields.put_str("model", "mock");
        fields.put_str("llm_transcript_dir", "selected");
        fields.put("llm_transcript_raw", VmValue::Bool(raw));
        let projected = crate::llm::helpers::project_llm_options(&fields).unwrap();
        let opts = crate::llm::helpers::extract_llm_options(&[
            VmValue::string("prompt"),
            VmValue::Nil,
            VmValue::dict(projected),
        ])
        .unwrap();
        assert_eq!(opts.llm_transcript_dir.as_deref(), Some("selected"));
        assert_eq!(opts.llm_transcript_raw, Some(raw));
        let payload = serde_json::to_value(api::LlmRequestPayload::from(&opts)).unwrap();
        assert!(payload.get("llm_transcript_dir").is_none());
        assert!(payload.get("llm_transcript_raw").is_none());
    }
    for (key, value) in [
        ("llm_transcript_raw", VmValue::string("false")),
        ("llm_transcript_raw", VmValue::Int(1)),
        ("llm_transcript_dir", VmValue::Bool(true)),
    ] {
        let mut fields = DictMap::new();
        fields.put_str("provider", "mock");
        fields.put_str("model", "mock");
        fields.put(key, value);
        let error = crate::llm::helpers::extract_llm_options(&[
            VmValue::string("prompt"),
            VmValue::Nil,
            VmValue::dict(fields),
        ])
        .unwrap_err();
        assert!(error.to_string().contains(key));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn per_call_capture_overrides_environment_and_keeps_redacted_request_response_ownership() {
    let _env = crate::llm::env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _dir = ScopedEnvVar::remove("HARN_LLM_TRANSCRIPT_DIR");
    let _cleanup = Cleanup;
    assert_ne!(
        crate::redact::current_policy()
            .redact_string(SECRET)
            .as_ref(),
        SECRET
    );
    for (index, (environment, raw, expected)) in [
        ("0", None, false),
        ("1", None, true),
        ("1", Some(false), false),
        ("0", Some(true), true),
    ]
    .into_iter()
    .enumerate()
    {
        let _raw = ScopedEnvVar::set("HARN_LLM_TRANSCRIPT_RAW", environment);
        let dir = tempfile::tempdir().unwrap();
        let marker = format!("capture-case-{index}");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (server, _seen, mut release) = stub(&marker, requests.clone());
        release.release();
        install_openai_stub_provider("openai", server.addr());
        let result = call(
            &options("openai", Some(dir.path()), raw, &marker),
            index % 2 == 1,
        )
        .await
        .unwrap();
        assert!(result.text.contains(&marker));
        let wire = requests.lock().unwrap();
        assert_eq!(wire.len(), 1, "real provider request must be reached");
        assert!(wire[0].get("llm_transcript_dir").is_none());
        assert!(wire[0].get("llm_transcript_raw").is_none());
        assert_capture(dir.path(), &marker, expected, &wire[0]);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn explicit_capture_without_directory_refuses_before_real_provider_dispatch() {
    let _env = crate::llm::env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _dir = ScopedEnvVar::remove("HARN_LLM_TRANSCRIPT_DIR");
    let _cleanup = Cleanup;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (server, _seen, mut release) = stub("not-reached", requests.clone());
    release.release();
    install_openai_stub_provider("openai", server.addr());
    let error = call(&options("openai", None, Some(true), "not-reached"), false)
        .await
        .unwrap_err();
    let VmError::Thrown(VmValue::Dict(fields)) = error else {
        panic!("structured refusal required")
    };
    assert_eq!(
        fields.get("reason_code").map(VmValue::display).as_deref(),
        Some("llm_transcript_directory_required")
    );
    assert_eq!(requests.lock().unwrap().len(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_capture_options_refuse_before_runtime_capability_http() {
    let _env = crate::llm::env_guard();
    let _cleanup = Cleanup;
    for malformed in [
        None,
        Some(("llm_transcript_raw", VmValue::string("yes"))),
        Some(("llm_transcript_dir", VmValue::Bool(true))),
    ] {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let observed = requests.clone();
        let server = spawn_llm_stub("capture preflight", move |stream| {
            use std::io::Write;
            let (headers, body) = read_http_request(stream);
            assert!(headers.starts_with("POST /api/show "));
            observed.lock().unwrap().push(body);
            let body = r#"{"capabilities":["tools"]}"#;
            write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        install_openai_stub_provider("ollama", server.addr());
        let mut fields = DictMap::new();
        fields.put_str("provider", "ollama");
        fields.put_str("model", "gpt-4o-mini");
        if let Some((key, value)) = malformed.clone() {
            fields.put(key, value);
        }
        let result = crate::llm::helpers::prepare_llm_options_safe(&[
            VmValue::string("preflight"),
            VmValue::Nil,
            VmValue::dict(fields),
        ])
        .await;
        if let Some((key, _)) = malformed {
            assert!(result.unwrap_err().to_string().contains(key));
            assert_eq!(requests.lock().unwrap().len(), 0);
        } else {
            result.unwrap();
            assert_eq!(
                requests.lock().unwrap().len(),
                1,
                "runtime capability path must be reached"
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn blocked_capture_directory_reports_failure_without_a_success_pointer_or_losing_answer() {
    let _env = crate::llm::env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _cleanup = Cleanup;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("raw-provider"), "occupied").unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (server, _seen, mut release) = stub("still-answered", requests.clone());
    release.release();
    install_openai_stub_provider("openai", server.addr());
    let result = call(
        &options("openai", Some(dir.path()), Some(true), "still-answered"),
        true,
    )
    .await
    .unwrap();
    assert!(result.text.contains("still-answered"));
    assert_eq!(requests.lock().unwrap().len(), 1);
    let events = transcript(dir.path());
    assert!(events
        .iter()
        .all(|event| event["type"] != "provider_raw_capture"));
    let failures: Vec<_> = events
        .iter()
        .filter(|event| event["type"] == "provider_raw_capture_failed")
        .collect();
    assert_eq!(failures.len(), 2);
    assert!(failures
        .iter()
        .all(|event| event["reason_code"] == "sidecar_write_failed"));
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_calls_keep_distinct_directories_across_real_transport_waits() {
    let _env = crate::llm::env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _dir = ScopedEnvVar::remove("HARN_LLM_TRANSCRIPT_DIR");
    let _cleanup = Cleanup;
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let a_requests = Arc::new(Mutex::new(Vec::new()));
    let b_requests = Arc::new(Mutex::new(Vec::new()));
    let (a_server, a_seen, mut a_release) = stub("call-a-owned", a_requests.clone());
    let (b_server, b_seen, mut b_release) = stub("call-b-owned", b_requests.clone());
    install_openai_stub_provider("capture-a", a_server.addr());
    let a_provider = crate::llm_config::provider_config("capture-a").unwrap();
    install_openai_stub_provider("capture-b", b_server.addr());
    let b_provider = crate::llm_config::provider_config("capture-b").unwrap();
    crate::llm_config::set_user_overrides(Some(crate::llm_config::ProvidersConfig {
        providers: std::collections::BTreeMap::from([
            ("capture-a".to_string(), a_provider),
            ("capture-b".to_string(), b_provider),
        ]),
        ..Default::default()
    }));
    let a_opts = options("capture-a", Some(a_dir.path()), Some(true), "call-a-owned");
    let b_opts = options("capture-b", Some(b_dir.path()), Some(true), "call-b-owned");
    let mut a = Box::pin(crate::orchestration::scope_ambient(
        crate::orchestration::AmbientExecutionScope::capture_inherited(),
        call(&a_opts, true),
    ));
    let mut b = Box::pin(crate::orchestration::scope_ambient(
        crate::orchestration::AmbientExecutionScope::capture_inherited(),
        call(&b_opts, true),
    ));
    let both_seen = async {
        a_seen.await.unwrap();
        b_seen.await.unwrap();
    };
    let (a_result, b_result, overlapped) = tokio::select! {
        result = &mut a => {
            a_release.release(); b_release.release();
            (result, b.await, false)
        }
        result = &mut b => {
            a_release.release(); b_release.release();
            (a.await, result, false)
        }
        () = both_seen => {
            a_release.release(); b_release.release();
            let (a_result, b_result) = tokio::join!(a, b);
            (a_result, b_result, true)
        }
    };
    assert!(a_result.unwrap().text.contains("call-a-owned"));
    assert!(b_result.unwrap().text.contains("call-b-owned"));
    assert!(
        overlapped,
        "both requests must arrive before either response is released"
    );
    assert_eq!(a_requests.lock().unwrap().len(), 1);
    assert_eq!(b_requests.lock().unwrap().len(), 1);
    assert_capture(
        a_dir.path(),
        "call-a-owned",
        true,
        &a_requests.lock().unwrap()[0],
    );
    assert_capture(
        b_dir.path(),
        "call-b-owned",
        true,
        &b_requests.lock().unwrap()[0],
    );
    assert!(
        !std::fs::read_to_string(a_dir.path().join("llm_transcript.jsonl"))
            .unwrap()
            .contains("call-b-owned")
    );
    assert!(
        !std::fs::read_to_string(b_dir.path().join("llm_transcript.jsonl"))
            .unwrap()
            .contains("call-a-owned")
    );
    assert!(
        current_transcript_dir().is_none(),
        "completed calls must restore ambient routing"
    );
}

#[test]
fn transcript_scope_keeps_identical_directory_definitions_and_transfers_session_ownership() {
    let _outer = TranscriptDirScope::push(Some("outer-capture"));
    assert!(system_prompt_changed(123));
    {
        let _same = TranscriptDirScope::push_if_changed(Some("outer-capture"));
        assert!(
            !system_prompt_changed(123),
            "same sink must retain deduplication state"
        );
        let _inner = TranscriptDirScope::push_if_changed(Some("inner-capture"));
        assert_eq!(current_transcript_dir().as_deref(), Some("inner-capture"));
    }
    assert_eq!(current_transcript_dir().as_deref(), Some("outer-capture"));
    let frame = TranscriptDirScope::push(Some("session-owned"))
        .into_frame()
        .unwrap();
    assert_eq!(current_transcript_dir().as_deref(), Some("session-owned"));
    assert!(remove_llm_transcript_dir(frame));
    assert_eq!(current_transcript_dir().as_deref(), Some("outer-capture"));
}
