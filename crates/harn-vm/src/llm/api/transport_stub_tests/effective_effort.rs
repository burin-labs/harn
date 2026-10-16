//! Echo and selection provenance through real Responses HTTP and SSE calls.

use super::*;
use crate::llm::api::{LlmApiMode, ReasoningEffort, ResolvedSetting};
use crate::llm::{EffectiveReasoningEffort, ReasoningEffortSource};

struct Cleanup;

impl Drop for Cleanup {
    fn drop(&mut self) {
        crate::llm_config::clear_runtime_provider_endpoint_overrides();
        crate::llm::agent_observe::pop_llm_transcript_dir();
        crate::llm::set_replay_mode(crate::llm::LlmReplayMode::Off, "");
    }
}

#[test]
fn completed_responses_report_effort_and_source_across_http_and_sse() {
    let _guard = env_guard();
    let _allow = allow_stubbed_llm_transport();
    let transcript_dir = tempfile::tempdir().expect("transcript dir");
    crate::llm::agent_observe::push_llm_transcript_dir(
        transcript_dir.path().to_str().expect("UTF-8 path"),
    );
    let _cleanup = Cleanup;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let cases = [
        (
            None,
            Some("medium"),
            None,
            Some(ReasoningEffortSource::ProviderDefault),
        ),
        (
            Some("high"),
            Some("high"),
            None,
            Some(ReasoningEffortSource::Operator),
        ),
        (Some("high"), None, None, None),
        (
            Some("low"),
            Some("high"),
            Some("high"),
            Some(ReasoningEffortSource::Operator),
        ),
        (
            Some("high"),
            Some("low"),
            None,
            Some(ReasoningEffortSource::ProviderAdjusted),
        ),
    ];
    let mut expected_events = Vec::new();
    for streamed in [false, true] {
        for (requested, echoed, overridden, source) in cases {
            let expected = match (echoed, source) {
                (Some(level), Some(source)) => EffectiveReasoningEffort::Reported {
                    level: level.into(),
                    source,
                },
                _ => EffectiveReasoningEffort::NotReported,
            };
            let expected_json = serde_json::to_value(&expected).expect("observation JSON");
            expected_events.push(expected_json.clone());
            let server = spawn_llm_stub("effort echo", move |stream| {
                use std::io::Write;
                let (headers, body) = super::ollama_openai_compat::read_http_request(stream);
                assert!(headers.starts_with("POST /v1/responses HTTP/1.1"));
                assert_eq!(
                    body.pointer("/reasoning/effort").and_then(|v| v.as_str()),
                    overridden.or(requested),
                    "assert the actual wire selection"
                );
                assert_eq!(
                    body.get("stream")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    streamed
                );
                let response = serde_json::json!({
                    "id": "resp_effort", "status": "completed",
                    "reasoning": {"effort": echoed},
                    "output": [{"type": "message", "role": "assistant",
                        "content": [{"type": "output_text", "text": "OK"}]}],
                    "usage": {"input_tokens": 9, "output_tokens": 1, "total_tokens": 10},
                });
                let (content_type, payload) = if streamed {
                    (
                        "text/event-stream",
                        format!(
                            "data: {}\n\ndata: {}\n\n",
                            serde_json::json!({"type": "response.output_text.delta", "delta": "OK"}),
                            serde_json::json!({"type": "response.completed", "response": response})
                        ),
                    )
                } else {
                    ("application/json", response.to_string())
                };
                write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}", payload.len())
                    .expect("write response");
            });
            crate::llm_config::set_runtime_provider_endpoint_overrides(
                crate::llm_config::RuntimeProviderEndpointOverrides::single(
                    "openai",
                    format!("http://{}/v1", server.addr()),
                )
                .expect("endpoint"),
            );
            let mut opts = base_opts("openai");
            opts.model = "gpt-5.4-mini".into();
            opts.api_mode = LlmApiMode::Responses;
            opts.stream = streamed;
            if let Some(level) = requested {
                opts.thinking = ThinkingConfig::Effort {
                    level: if level == "high" {
                        ReasoningEffort::High
                    } else {
                        ReasoningEffort::Low
                    },
                };
                opts.resolution.push(ResolvedSetting {
                    setting: "reasoning",
                    source: "caller.effort",
                    ..Default::default()
                });
            }
            if let Some(level) = overridden {
                opts.provider_overrides = Some(serde_json::json!({"reasoning": {"effort": level}}));
            }
            let mut result = runtime.block_on(async {
                if streamed {
                    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                    let result = vm_call_llm_full_streaming(&opts, tx)
                        .await
                        .expect("SSE call");
                    assert_eq!(rx.try_recv().expect("visible delta"), "OK");
                    result
                } else {
                    vm_call_llm_full(&opts).await.expect("HTTP call")
                }
            });
            assert_eq!(result.text, "OK");
            assert_eq!(result.telemetry.effective_reasoning_effort, expected);
            // The observation must survive even when a provider omits its stop reason.
            result.stop_reason = None;
            let value = crate::llm::api::vm_build_llm_result(
                &result,
                None,
                None,
                &crate::llm::api::test_text_projection(&result, None),
            );
            let json = crate::llm::vm_value_to_json(&value);
            assert_eq!(json["effective_reasoning_effort"], expected_json);
            let durable = crate::llm::pairing_receipts::attach_assistant_facts(
                crate::schema::json_to_vm_value(
                    &serde_json::json!({"role": "assistant", "content": "OK"}),
                ),
                &value,
            );
            assert_eq!(
                crate::llm::vm_value_to_json(&durable)["_harn"]["effective_reasoning_effort"],
                expected_json
            );
            assert_eq!(
                json["usage"]["provider_telemetry"]["effective_reasoning_effort"],
                expected_json
            );
            crate::llm::set_replay_mode(
                crate::llm::LlmReplayMode::Record,
                transcript_dir.path().to_str().expect("UTF-8 path"),
            );
            crate::llm::hash_replay::save_fixture("effort", &result);
            let replayed = crate::llm::hash_replay::load_fixture("effort").expect("fixture");
            assert_eq!(replayed.telemetry.effective_reasoning_effort, expected);
            crate::llm::set_replay_mode(crate::llm::LlmReplayMode::Off, "");
            drop(server);
        }
    }
    let events: Vec<serde_json::Value> =
        std::fs::read_to_string(transcript_dir.path().join("llm_transcript.jsonl"))
            .expect("transcript")
            .lines()
            .map(|line| serde_json::from_str(line).expect("event JSON"))
            .filter(|event: &serde_json::Value| event["type"] == "provider_call_response")
            .collect();
    assert_eq!(
        events.len(),
        expected_events.len(),
        "every completed call reported"
    );
    for (event, expected) in events.iter().zip(expected_events) {
        assert_eq!(event["effective_reasoning_effort"], expected);
    }
}
