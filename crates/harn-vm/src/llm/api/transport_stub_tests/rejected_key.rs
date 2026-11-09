//! A key the provider rejected is not sent again by the same process until the
//! retry window passes, and the local refusal reads exactly like the rejection.
use super::*;
use crate::llm::api::{classify_vm_llm_error, vm_call_completion_full, LlmCallOptions};
use crate::llm::api::{LlmErrorKind, LlmErrorReason};
use crate::llm::rate_limit::REJECTED_KEY_RETRY_AFTER;
use crate::value::{ErrorCategory, VmError, VmValue};
use serde_json::{json, Value};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

mod admission_edges;

const PROVIDER: &str = "rejected-key";
const KEY_ENV: &str = "REJECTED_KEY_TEST_API_KEY";

struct Cleanup;
impl Drop for Cleanup {
    fn drop(&mut self) {
        crate::reset_thread_local_state();
        crate::llm_config::clear_user_overrides();
    }
}

/// Answers 401 to every request unless `accept` is set, and records each
/// request's `Authorization` header.
fn auth_stub(seen: Arc<Mutex<Vec<String>>>, accept: Arc<AtomicBool>) -> LlmStub {
    gated_auth_stub(seen, accept, None)
}

fn gated_auth_stub(
    seen: Arc<Mutex<Vec<String>>>,
    accept: Arc<AtomicBool>,
    mut first_body_gate: Option<(
        tokio::sync::oneshot::Sender<()>,
        std::sync::mpsc::Receiver<()>,
    )>,
) -> LlmStub {
    spawn_llm_stub_many("rejected key", 6, move |_, stream| {
        let (headers, _) = ollama_openai_compat::read_http_request(stream);
        assert!(headers.starts_with("POST /chat/completions "), "{headers}");
        let authorization = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("authorization")
                    .then(|| value.trim().to_string())
            })
            .unwrap_or_default();
        seen.lock().unwrap().push(authorization);
        let (status, body) = if accept.load(Ordering::SeqCst) {
            (
                "200 OK",
                json!({"id":"served", "model":"rejected-key-model",
                    "choices":[{"index":0, "message":{"role":"assistant", "content":"served"},
                        "finish_reason":"stop"}],
                    "usage":{"prompt_tokens":3, "completion_tokens":1}})
                .to_string(),
            )
        } else {
            (
                "401 Unauthorized",
                json!({"error":{"message":"Incorrect API key provided",
                    "type":"authentication_error"}})
                .to_string(),
            )
        };
        write!(stream, "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len()).unwrap();
        stream.flush().unwrap();
        if let Some((received, release)) = first_body_gate.take() {
            assert!(received.send(()).is_ok());
            release.recv().unwrap();
            // An assertion failure can cancel the client before the body gate opens.
            let _ = stream.write_all(body.as_bytes());
        } else {
            stream.write_all(body.as_bytes()).unwrap();
        }
    })
}

fn install(addr: std::net::SocketAddr, auth_env: crate::llm_config::AuthEnv) {
    let mut config = crate::llm_config::ProvidersConfig::default();
    config.providers.insert(
        PROVIDER.into(),
        crate::llm_config::ProviderDef {
            base_url: format!("http://{addr}"),
            chat_endpoint: "/chat/completions".into(),
            completion_endpoint: Some("/completions".into()),
            auth_style: "bearer".into(),
            auth_env,
            ..Default::default()
        },
    );
    crate::llm_config::set_user_overrides(Some(config));
}

fn options(api_key: &str) -> LlmCallOptions {
    LlmCallOptions {
        provider: PROVIDER.into(),
        model: "rejected-key-model".into(),
        api_key: api_key.into(),
        max_tokens: 8,
        stream: false,
        messages: vec![json!({"role":"user", "content":"hello"})],
        ..Default::default()
    }
}

fn field(error: &VmError, name: &str) -> Option<VmValue> {
    match error {
        VmError::Thrown(VmValue::Dict(fields)) => fields.get(name).cloned(),
        _ => None,
    }
}

fn assert_auth_failure(error: &VmError) {
    assert_eq!(
        crate::value::error_to_category(error),
        ErrorCategory::Auth,
        "{error}"
    );
    let classified = classify_vm_llm_error(error);
    assert_eq!(classified.kind, LlmErrorKind::Terminal, "{error}");
    assert_eq!(classified.reason, LlmErrorReason::AuthFailure, "{error}");
}

fn assert_refused_locally(error: &VmError) {
    assert_auth_failure(error);
    assert_eq!(
        field(error, "attempt_count").and_then(|v| v.as_int()),
        Some(0)
    );
    let message = error.to_string();
    assert!(
        message.contains("already rejected this API key in this process"),
        "{message}"
    );
    assert!(
        message.contains("HTTP 401"),
        "keeps the provider's status: {message}"
    );
    assert!(
        !message.contains("key-a"),
        "never echoes the key: {message}"
    );
}

#[test]
fn rejected_key_is_refused_locally_until_its_retry_window_passes() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _durable = ScopedEnvVar::set("HARN_LLM_RATE_LIMIT_DURABLE", "0");
    crate::reset_thread_local_state();
    let _cleanup = Cleanup;
    let _clock =
        crate::clock_mock::install_override(crate::clock_mock::MockClock::at_wall_ms(1_000));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let accept = Arc::new(AtomicBool::new(false));
        let server = auth_stub(seen.clone(), accept.clone());
        install(server.addr(), crate::llm_config::AuthEnv::None);
        let sent = || seen.lock().unwrap().clone();

        let first = vm_call_llm_full(&options("key-a")).await.unwrap_err();
        assert_auth_failure(&first);
        assert!(field(&first, "attempt_count").is_none(), "a real rejection");
        assert_eq!(
            field(&first, "credential_scope").map(|v| v.display()),
            Some("key".to_string()),
            "a 401 rejects the key itself"
        );
        assert_eq!(
            sent(),
            ["Bearer key-a"],
            "first call reaches the provider once"
        );

        let second = vm_call_llm_full(&options("key-a")).await.unwrap_err();
        assert_refused_locally(&second);
        let completion = vm_call_completion_full(&options("key-a"), "prefix", Some("suffix"))
            .await
            .unwrap_err();
        assert_refused_locally(&completion);
        assert_eq!(
            sent().len(),
            1,
            "neither chat nor completion re-sent the key"
        );

        let other_key = vm_call_llm_full(&options("key-b")).await.unwrap_err();
        assert_auth_failure(&other_key);
        assert!(field(&other_key, "attempt_count").is_none());
        assert_eq!(
            sent(),
            ["Bearer key-a", "Bearer key-b"],
            "a different key is sent"
        );

        crate::clock_mock::advance(
            REJECTED_KEY_RETRY_AFTER
                .checked_sub(std::time::Duration::from_secs(1))
                .unwrap(),
        );
        assert_refused_locally(&vm_call_llm_full(&options("key-a")).await.unwrap_err());
        assert_eq!(sent().len(), 2, "still inside the retry window");

        crate::clock_mock::advance(std::time::Duration::from_secs(1));
        accept.store(true, Ordering::SeqCst);
        let served = vm_call_llm_full(&options("key-a")).await.unwrap();
        assert_eq!(served.text, "served");
        assert_eq!(sent().len(), 3, "the key is sent again after the window");
        assert_eq!(sent()[2], "Bearer key-a");
    });
}

/// Answers 403 to requests for `forbidden-model` and serves every other model,
/// recording each request's model.
fn model_access_stub(
    seen: Arc<Mutex<Vec<String>>>,
    message: &'static str,
    error_type: &'static str,
) -> LlmStub {
    spawn_llm_stub_many("model access", 4, move |_, stream| {
        let (_, request) = ollama_openai_compat::read_http_request(stream);
        let model = request["model"].as_str().unwrap_or_default().to_string();
        let (status, body) = if model == "forbidden-model" {
            (
                "403 Forbidden",
                json!({"error":{"message":message,
                    "type":error_type}})
                .to_string(),
            )
        } else {
            (
                "200 OK",
                json!({"id":"served", "model":model,
                    "choices":[{"index":0, "message":{"role":"assistant", "content":"served"},
                        "finish_reason":"stop"}],
                    "usage":{"prompt_tokens":3, "completion_tokens":1}})
                .to_string(),
            )
        };
        seen.lock().unwrap().push(model);
        write!(stream, "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
    })
}

#[test]
fn a_model_access_refusal_blocks_only_that_model_for_the_key() {
    assert_model_access_refusal(
        "Project does not have access to model forbidden-model",
        "permission_error",
    );
}

fn assert_model_access_refusal(message: &'static str, error_type: &'static str) {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _durable = ScopedEnvVar::set("HARN_LLM_RATE_LIMIT_DURABLE", "0");
    crate::reset_thread_local_state();
    let _cleanup = Cleanup;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let server = model_access_stub(seen.clone(), message, error_type);
        install(server.addr(), crate::llm_config::AuthEnv::None);
        let on_model = |model: &str| LlmCallOptions {
            model: model.into(),
            ..options("model-scoped-key")
        };
        let sent = || seen.lock().unwrap().clone();

        let forbidden = vm_call_llm_full(&on_model("forbidden-model"))
            .await
            .unwrap_err();
        assert_auth_failure(&forbidden);
        assert_eq!(
            field(&forbidden, "credential_scope").map(|v| v.display()),
            Some("request".to_string()),
            "a 403 without key-rejection wording names the request: {forbidden}"
        );

        let fallback = vm_call_llm_full(&on_model("allowed-model")).await.unwrap();
        assert_eq!(fallback.text, "served");
        assert_eq!(
            sent(),
            ["forbidden-model", "allowed-model"],
            "the same key on another model is sent and served"
        );

        let repeat = vm_call_llm_full(&on_model("forbidden-model"))
            .await
            .unwrap_err();
        assert_auth_failure(&repeat);
        assert_eq!(
            field(&repeat, "attempt_count").and_then(|v| v.as_int()),
            Some(0)
        );
        assert!(
            repeat
                .to_string()
                .contains("already rejected this API key for 'forbidden-model'"),
            "{repeat}"
        );
        assert_eq!(sent().len(), 2, "the refused model is not re-sent");
    });
}

fn run_classify_then_loop(classify_first: bool) -> Value {
    let source = include_str!("rejected_key/classify_then_loop.harn")
        .replace("__CLASSIFY_FIRST__", &classify_first.to_string());
    let path = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/llm/api/transport_stub_tests/rejected_key/classify_then_loop.harn"
    ));
    let chunk = crate::compile_source_at_path(&source, path, None).expect("fixture compiles");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let output = runtime.block_on(tokio::task::LocalSet::new().run_until(async {
        let mut vm = crate::new_vm();
        crate::register_vm_stdlib(&mut vm);
        vm.execute(&chunk).await.expect("pipeline finishes");
        vm.output().to_string()
    }));
    output
        .lines()
        .filter_map(|line| line.strip_prefix("[harn] "))
        .find_map(|line| serde_json::from_str(line).ok())
        .unwrap_or_else(|| panic!("fixture report in: {output}"))
}

/// The agent loop's terminal outcome, minus the fields that say whether the
/// refusal was remote or local.
fn loop_outcome(report: &Value) -> Value {
    let mut outcome = report["loop"].clone();
    if let Some(fields) = outcome.get_mut("error").and_then(Value::as_object_mut) {
        fields.remove("message");
        fields.remove("attempt_count");
        fields.remove("provider_call_count");
    }
    outcome
}

#[test]
fn agent_loop_after_a_rejected_classifier_stops_without_a_second_request() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _durable = ScopedEnvVar::set("HARN_LLM_RATE_LIMIT_DURABLE", "0");
    crate::reset_thread_local_state();
    let _cleanup = Cleanup;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let server = auth_stub(seen.clone(), Arc::new(AtomicBool::new(false)));
    install(
        server.addr(),
        crate::llm_config::AuthEnv::Single(KEY_ENV.into()),
    );

    // Control: the agent loop alone meets the provider's real 401.
    let real = {
        let _key = ScopedEnvVar::set(KEY_ENV, "loop-only-key");
        run_classify_then_loop(false)
    };
    assert_eq!(*seen.lock().unwrap(), ["Bearer loop-only-key"]);

    let refused = {
        let _key = ScopedEnvVar::set(KEY_ENV, "classify-first-key");
        run_classify_then_loop(true)
    };
    assert_eq!(
        *seen.lock().unwrap(),
        ["Bearer loop-only-key", "Bearer classify-first-key"],
        "the classifier's rejection kept the agent loop off the network"
    );
    assert_eq!(refused["classifier"]["ok"], false, "{refused}");
    assert_eq!(refused["classifier"]["error_category"], "auth", "{refused}");
    assert_eq!(real["loop"]["error"]["provider_call_count"], 1, "{real}");
    assert_eq!(
        refused["loop"]["error"]["provider_call_count"], 0,
        "{refused}"
    );
    assert_eq!(refused["loop"]["error"]["attempt_count"], 0, "{refused}");
    assert_eq!(
        loop_outcome(&refused),
        loop_outcome(&real),
        "the loop ends the same way as on a real rejection\nreal: {real}\nrefused: {refused}"
    );
}
