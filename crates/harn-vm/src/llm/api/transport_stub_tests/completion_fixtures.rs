use super::*;
use crate::llm::api::{vm_call_completion_full, LlmCallOptions};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct Cleanup;

impl Drop for Cleanup {
    fn drop(&mut self) {
        crate::reset_thread_local_state();
        crate::llm_config::clear_user_overrides();
    }
}

#[test]
fn completion_rejects_project_invented_no_training_fact() {
    struct ClearCatalog;
    impl Drop for ClearCatalog {
        fn drop(&mut self) {
            crate::llm_config::clear_runtime_catalog_overlay();
        }
    }
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let _cleanup = Cleanup;
    let _catalog = ClearCatalog;
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = requests.clone();
    let server = spawn_llm_stub_many("completion privacy authority", 2, move |_, stream| {
        read_http_request(stream);
        seen.fetch_add(1, Ordering::SeqCst);
        let response = r#"{"choices":[{"text":"accepted","finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1}}"#;
        write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}", response.len()).unwrap();
    });
    let mut provider = crate::llm_config::provider_config("openai").unwrap();
    provider.base_url = format!("http://{}/v1", server.addr());
    provider.base_url_env = None;
    provider.completion_endpoint = Some("/completions".into());
    provider.data_controls.as_mut().unwrap().training_default =
        crate::llm_config::TrainingDefault::DoesNotTrain;
    let mut host = crate::llm_config::ProvidersConfig::default();
    host.providers.insert("openai".into(), provider.clone());
    crate::llm_config::set_runtime_catalog_overlay(Some(host));
    let mut opts = base_opts("openai");
    opts.model = "gpt-5.6-luna".into();
    opts.api_key = "synthetic".into();
    opts.stream = false;
    opts.inference_boundary = Some(crate::llm::api::InferenceBoundary {
        reach: crate::llm::api::InferenceReach::AnyHosted,
        allow_training_discounts: false,
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    assert_eq!(
        runtime
            .block_on(vm_call_completion_full(&opts, "prefix", None))
            .unwrap()
            .text,
        "accepted"
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "trusted route reached the listener"
    );
    let mut overridden = opts.clone();
    overridden.provider_overrides = Some(serde_json::json!({"model": "project-overridden-model"}));
    assert!(
        runtime
            .block_on(vm_call_completion_full(&overridden, "prefix", None))
            .is_err(),
        "completion cannot replace its admitted model"
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "model override sent no completion request"
    );
    provider.data_controls.as_mut().unwrap().training_default =
        crate::llm_config::TrainingDefault::Unspecified;
    let mut host = crate::llm_config::ProvidersConfig::default();
    host.providers.insert("openai".into(), provider.clone());
    crate::llm_config::set_runtime_catalog_overlay(Some(host));
    provider.data_controls.as_mut().unwrap().training_default =
        crate::llm_config::TrainingDefault::DoesNotTrain;
    let mut project = crate::llm_config::ProvidersConfig::default();
    project.providers.insert("openai".into(), provider);
    crate::llm_config::set_user_overrides(Some(project));
    assert!(runtime
        .block_on(vm_call_completion_full(&opts, "prefix", None))
        .is_err());
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "forged fact sent no completion request"
    );
}

fn assert_completion_fixture(endpoint: Option<&str>) {
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
        let requests = Arc::new(AtomicUsize::new(0));
        let seen = requests.clone();
        let path = endpoint.unwrap_or("/chat/completions").to_owned();
        let ollama = endpoint == Some("/api/generate");
        let chat = endpoint.is_none();
        let server = spawn_llm_stub_many("completion fixture control", 2, move |_, stream| {
            let (headers, body) = read_http_request(stream);
            assert!(headers.starts_with(&format!("POST {path} ")));
            assert_eq!(body["model"], "completion-model");
            if !chat {
                assert_eq!(body["prompt"], "prefix");
                assert_eq!(body["suffix"], "suffix");
            }
            seen.fetch_add(1, Ordering::SeqCst);
            let response = if ollama {
                serde_json::json!({"response":"provider-reached", "done":true,
                    "prompt_eval_count":3, "eval_count":1})
            } else {
                let choice = if chat {
                    serde_json::json!({"message":{"role":"assistant", "content":"provider-reached"}, "finish_reason":"stop"})
                } else {
                    serde_json::json!({"text":"provider-reached", "finish_reason":"stop"})
                };
                serde_json::json!({"id":"completion-control", "model":"completion-model",
                    "choices":[choice], "usage":{"prompt_tokens":3, "completion_tokens":1}})
            }.to_string();
            write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}", response.len()).unwrap();
        });
        let provider = "completion-fixture";
        let mut config = crate::llm_config::ProvidersConfig::default();
        config.providers.insert(provider.into(), crate::llm_config::ProviderDef {
            base_url: format!("http://{}", server.addr()),
            chat_endpoint: "/chat/completions".into(),
            completion_endpoint: endpoint.map(str::to_owned),
            auth_style: "none".into(),
            auth_env: crate::llm_config::AuthEnv::None,
            ..Default::default()
        });
        crate::llm_config::set_user_overrides(Some(config));
        crate::llm::install_cli_llm_mock_fixture(crate::llm::parse_llm_mocks_jsonl(
            "{\"schemaVersion\":1,\"strictScopes\":true}\n\
             {\"id\":\"completion-1\",\"scope\":\"completion.fixture\",\"consume\":\"once\",\"match\":\"*prefix*\",\"text\":\"fixture-reached\",\"input_tokens\":7,\"output_tokens\":2}\n"
        ).unwrap());
        let opts = LlmCallOptions {
            provider: provider.into(),
            model: "completion-model".into(),
            mock_scope: Some("completion.fixture".into()),
            max_tokens: 8,
            stream: false,
            ..Default::default()
        };
        let fixture = vm_call_completion_full(&opts, "prefix", Some("suffix"))
            .await
            .unwrap();
        assert_eq!(fixture.text, "fixture-reached");
        assert_eq!((fixture.input_tokens, fixture.output_tokens), (7, 2));
        assert_eq!(requests.load(Ordering::SeqCst), 0);
        let exhausted = vm_call_completion_full(&opts, "prefix", Some("suffix"))
            .await
            .expect_err("an exhausted replay must not reach the provider");
        assert!(exhausted.to_string().contains("No --llm-mock fixture matched"));
        assert_eq!(requests.load(Ordering::SeqCst), 0);

        crate::llm::clear_cli_llm_mock_mode();
        crate::llm::enable_cli_llm_mock_recording();
        let live = vm_call_completion_full(&opts, "prefix", Some("suffix"))
            .await
            .unwrap();
        assert_eq!(live.text, "provider-reached");
        assert_eq!(requests.load(Ordering::SeqCst), 1, "same route's positive control");
        let recordings = crate::llm::take_cli_llm_recordings();
        assert_eq!(recordings.len(), 1, "native and fallback results record once");
        assert_eq!(recordings[0].text, "provider-reached");
        assert_eq!(recordings[0].scope, "completion.fixture");
        assert_eq!(recordings[0].input_tokens, Some(3));
        assert_eq!(recordings[0].output_tokens, Some(1));

        crate::llm::clear_cli_llm_mock_mode();
        crate::llm::install_cli_llm_mock_fixture(crate::llm::parse_llm_mocks_jsonl(
            "{\"schemaVersion\":1,\"strictScopes\":true,\"liveAfterCalls\":1}\n\
             {\"id\":\"prefix-1\",\"scope\":\"completion.fixture\",\"consume\":\"once\",\"text\":\"prefix-reached\"}\n"
        ).unwrap());
        let prefix = vm_call_completion_full(&opts, "prefix", Some("suffix"))
            .await
            .unwrap();
        assert_eq!(prefix.text, "prefix-reached");
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(
            prefix.telemetry.llm_mock_prefix.as_deref(),
            Some(&crate::llm::mock::LlmMockPrefixMarker {
                served_by: crate::llm::mock::LlmMockPrefixServedBy::Fixture,
                call: 1,
                live_after_calls: 1,
            })
        );
        let handed_off = vm_call_completion_full(&opts, "prefix", Some("suffix"))
            .await
            .unwrap();
        assert_eq!(handed_off.text, "provider-reached");
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        assert_eq!(
            handed_off.telemetry.llm_mock_prefix.as_deref(),
            Some(&crate::llm::mock::LlmMockPrefixMarker {
                served_by: crate::llm::mock::LlmMockPrefixServedBy::Live,
                call: 2,
                live_after_calls: 1,
            })
        );
    });
}

#[test]
fn openai_completion_replays_without_io_and_records_the_live_control_once() {
    assert_completion_fixture(Some("/completions"));
}

#[test]
fn ollama_completion_replays_without_io_and_records_the_live_control_once() {
    assert_completion_fixture(Some("/api/generate"));
}

#[test]
fn chat_completion_fallback_replays_without_io_and_records_the_live_control_once() {
    assert_completion_fixture(None);
}
