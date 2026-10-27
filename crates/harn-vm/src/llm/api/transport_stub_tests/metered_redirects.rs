//! Local HTTP controls for redirect policy, cache isolation and durable admission.
use super::*;
use crate::llm::{MachineSpendPolicy, MachineSpendQuota};
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn quota(path: &std::path::Path, limit: i64) -> MachineSpendQuota {
    MachineSpendQuota::open(
        path,
        "redirect-fixture",
        MachineSpendPolicy {
            daily_limit_microusd: None,
            monthly_limit_microusd: None,
            lifetime_limit_microusd: Some(limit),
        },
    )
    .unwrap()
}

fn request(stream: &mut std::net::TcpStream) -> (String, Vec<u8>) {
    let mut reader = BufReader::new(stream);
    let mut start = String::new();
    reader.read_line(&mut start).unwrap();
    let mut length = 0;
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    (start, body)
}

fn redirect(stream: &mut std::net::TcpStream, location: &str) {
    write!(stream, "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
}

#[test]
fn endpoint_specific_data_policy_disables_redirects_without_a_spend_budget() {
    let _env = env_guard();
    let egress = crate::egress::test_support::test_env_guard();
    egress.set("HARN_EGRESS_ALLOW_LOOPBACK", "1");
    let _cleanup = Cleanup;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for factory in [
            crate::llm::blocking_client_for_base_url as fn(&str) -> reqwest::Client,
            crate::llm::streaming_client_for_base_url,
        ] {
            let followed = Arc::new(AtomicUsize::new(0));
            let count = followed.clone();
            let source = spawn_llm_stub_many("endpoint policy redirect", 5, move |_, stream| {
                let (start, body) = request(stream);
                if start.starts_with("POST /redirected ") {
                    assert_eq!(body, b"synthetic content");
                    count.fetch_add(1, Ordering::SeqCst);
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                    )
                    .unwrap();
                } else {
                    write!(stream, "HTTP/1.1 307 Temporary Redirect\r\nLocation: /redirected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                }
            });
            let base = format!("http://{}", source.addr());
            let url = format!("{base}/start");
            assert_eq!(factory(&base).post(&url).body("synthetic content").send().await.unwrap().status(), 200);
            assert_eq!(followed.load(Ordering::SeqCst), 1);
            let overlay = format!(
                concat!(
                    "[providers.fixture.regions.safe]\nbase_url = \"{base}\"\n",
                    "[providers.fixture.regions.safe.data_controls]\n",
                    "training_default = \"does_not_train\"\nchecked_on = \"2026-10-27\"\n",
                    "sources = [\"https://example.com/policy\"]\n"
                ),
                base = base
            );
            crate::llm_config::set_user_overrides(Some(toml::from_str(&overlay).unwrap()));
            assert_eq!(factory(&base).post(&url).body("synthetic content").send().await.unwrap().status(), 307);
            assert_eq!(followed.load(Ordering::SeqCst), 1);
            crate::llm_config::clear_user_overrides();
            assert_eq!(factory(&base).post(&url).body("synthetic content").send().await.unwrap().status(), 200);
            assert_eq!(followed.load(Ordering::SeqCst), 2);
        }
    });
}

#[test]
fn metered_inference_clients_isolate_redirect_policy_in_both_cache_directions() {
    let _env = env_guard();
    let egress = crate::egress::test_support::test_env_guard();
    // Fixture-only loopback access lets the ordinary policy follow both forms
    // of redirect; its positive control prevents SSRF rejection masking a bug.
    egress.set("HARN_EGRESS_ALLOW_LOOPBACK", "1");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let quota = quota(&temp.path().join("spend.sqlite"), 2_000_000);
        for factory in [
            crate::llm::blocking_client_for_base_url as fn(&str) -> reqwest::Client,
            crate::llm::streaming_client_for_base_url,
        ] {
            for cross_host in [false, true] {
                let destinations = Arc::new(AtomicUsize::new(0));
                let target_count = destinations.clone();
                let target = spawn_llm_stub_many("redirect target", 2, move |_, stream| {
                    request(stream);
                    target_count.fetch_add(1, Ordering::SeqCst);
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                    )
                    .unwrap();
                });
                let target_url = format!("http://localhost:{}/redirected", target.addr().port());
                let source_count = destinations.clone();
                let source = spawn_llm_stub_many("redirect source", 5, move |_, stream| {
                    let (start, _) = request(stream);
                    if start.starts_with("GET /redirected ") {
                        source_count.fetch_add(1, Ordering::SeqCst);
                        write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                        )
                        .unwrap();
                    } else {
                        redirect(
                            stream,
                            if cross_host {
                                &target_url
                            } else {
                                "/redirected"
                            },
                        );
                    }
                });
                let url = format!("http://{}/start", source.addr());
                // Warm the ordinary cache first, then prove the metered client
                // neither reuses it nor poisons a later ordinary lookup.
                assert_eq!(factory(&url).get(&url).send().await.unwrap().status(), 200);
                assert_eq!(destinations.load(Ordering::SeqCst), 1);
                let status = quota
                    .scope(async { factory(&url).get(&url).send().await.unwrap().status() })
                    .await
                    .unwrap();
                assert_eq!(status, 302);
                assert_eq!(destinations.load(Ordering::SeqCst), 1);
                assert_eq!(factory(&url).get(&url).send().await.unwrap().status(), 200);
                assert_eq!(destinations.load(Ordering::SeqCst), 2);
            }
        }
    });
}

#[test]
fn host_budget_without_a_machine_quota_also_refuses_redirects() {
    let _env = env_guard();
    let egress = crate::egress::test_support::test_env_guard();
    egress.set("HARN_EGRESS_ALLOW_LOOPBACK", "1");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let followed = Arc::new(AtomicUsize::new(0));
        let followed_count = followed.clone();
        let source = spawn_llm_stub_many("redirect source", 4, move |_, stream| {
            let (start, _) = request(stream);
            if start.starts_with("GET /redirected ") {
                followed_count.fetch_add(1, Ordering::SeqCst);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                )
                .unwrap();
            } else {
                redirect(stream, "/redirected");
            }
        });
        let url = format!("http://{}/start", source.addr());
        let budget = crate::llm::ConservativeLlmBudget::new(1.0).unwrap();
        let status = budget
            .scope(async {
                crate::llm::blocking_client_for_base_url(&url)
                    .get(&url)
                    .send()
                    .await
                    .unwrap()
                    .status()
            })
            .await
            .unwrap();
        assert_eq!(status, 302);
        assert_eq!(followed.load(Ordering::SeqCst), 0);
    });
}

struct Cleanup;
impl Drop for Cleanup {
    fn drop(&mut self) {
        crate::llm_config::clear_user_overrides();
        crate::llm_config::clear_runtime_provider_endpoint_overrides();
        crate::llm::cost::reset_cost_state();
    }
}

#[test]
fn metered_native_calls_reject_redirects_and_retain_unknown_usage_reservations() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let egress = crate::egress::test_support::test_env_guard();
    egress.set("HARN_EGRESS_ALLOW_LOOPBACK", "1");
    let _cleanup = Cleanup;
    crate::llm::cost::reset_cost_state();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for stream_response in [false, true] {
            let target_count = Arc::new(AtomicUsize::new(0));
            let reached_target = target_count.clone();
            let target = spawn_llm_stub("forbidden metered redirect target", move |stream| {
                request(stream);
                reached_target.fetch_add(1, Ordering::SeqCst);
                write!(stream, "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            });
            let location = format!("http://localhost:{}/redirected", target.addr().port());
            let source_count = Arc::new(AtomicUsize::new(0));
            let reached_source = source_count.clone();
            let source = spawn_llm_stub_many("metered native redirect", 3, move |_, stream| {
                let (start, body) = request(stream);
                assert!(start.starts_with("POST /v1/chat/completions "));
                let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(body["tools"][0]["function"]["name"], "echo");
                assert_eq!(body["stream"].as_bool().unwrap_or(false), stream_response);
                reached_source.fetch_add(1, Ordering::SeqCst);
                redirect(stream, &location);
            });
            install_openai_stub_provider("openai", source.addr());
            crate::llm_config::set_runtime_provider_endpoint_overrides(
                crate::llm_config::RuntimeProviderEndpointOverrides::single(
                    "openai", format!("http://{}/v1", source.addr())
                ).unwrap(),
            );
            let opts = super::super::LlmCallOptions {
                provider: "openai".into(), model: "gpt-5.6-luna".into(),
                messages: vec![serde_json::json!({"role":"user","content":"echo hello"})],
                max_tokens: 64, stream: stream_response,
                native_tools: Some(vec![serde_json::json!({"type":"function","function":{
                    "name":"echo","description":"Echo a synthetic value",
                    "parameters":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}
                }})]),
                parallel_tool_calls: Some(false),
                ..Default::default()
            };
            let temp = tempfile::tempdir().unwrap();
            let denied = quota(&temp.path().join("denied.sqlite"), 0);
            let error = denied.scope(vm_call_llm_full(&opts)).await.unwrap().unwrap_err();
            assert!(error.to_string().contains("machine spend quota"), "{error}");
            assert_eq!(source_count.load(Ordering::SeqCst), 0, "quota denies before I/O");
            let path = temp.path().join("spend.sqlite");
            let admitted = quota(&path, 2_000_000);
            let error = admitted.scope(vm_call_llm_full(&opts)).await.unwrap().unwrap_err();
            assert!(error.to_string().contains("302"), "{error}");
            assert_eq!(source_count.load(Ordering::SeqCst), 1);
            assert_eq!(target_count.load(Ordering::SeqCst), 0);
            let receipt = admitted.receipt().unwrap();
            assert!(receipt.lifetime_reserved_microusd > 0);
            assert_eq!(receipt.lifetime_actual_known_microusd, 0);
            assert_eq!(receipt.lifetime_usage_unknown_attempts, 1);
            assert!(!receipt.contract_broken);
            drop(admitted);
            assert_eq!(quota(&path, 2_000_000).receipt().unwrap(), receipt,
                "reopening must preserve uncertain reservations");
        }
    });
}

#[test]
fn metered_direct_provider_calls_settle_total_output_including_reasoning() {
    let _env = env_guard();
    let _transport = allow_stubbed_llm_transport();
    let egress = crate::egress::test_support::test_env_guard();
    egress.set("HARN_EGRESS_ALLOW_LOOPBACK", "1");
    let _cleanup = Cleanup;
    let _fixture_endpoint = crate::llm::admission::allow_fixture_endpoint();
    crate::llm::cost::reset_cost_state();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for (provider, model) in [
            ("fireworks", "accounts/fireworks/models/gpt-oss-120b"),
            ("groq", "groq/openai/gpt-oss-120b"),
            ("fireworks", "accounts/fireworks/models/glm-5p3-flash"),
            ("fireworks", "accounts/fireworks/models/deepseek-v4p1-flash"),
        ] {
            for streaming in [false, true] {
                for complete_usage in [false, true] {
                    let server = spawn_llm_stub("direct metered usage", move |stream| {
                        let (_, body) = request(stream);
                        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        assert_eq!(body["stream"].as_bool().unwrap_or(false), streaming);
                        assert!(body["max_tokens"] == 64 || body["max_completion_tokens"] == 64);
                        let usage = if complete_usage {
                            serde_json::json!({
                                "prompt_tokens": 8, "completion_tokens": 12, "total_tokens": 20,
                                "prompt_tokens_details": {"cached_tokens": 4},
                                "completion_tokens_details": {"reasoning_tokens": 7}
                            })
                        } else {
                            serde_json::Value::Null
                        };
                        let response = if streaming {
                            let chunk = serde_json::json!({
                                "id": "synthetic", "model": body["model"],
                                "choices": [{"index": 0, "delta": {"content": "ok", "reasoning_content": "synthetic reasoning"}, "finish_reason": "stop"}],
                                "usage": usage
                            });
                            format!("data: {chunk}\n\ndata: [DONE]\n\n")
                        } else {
                            serde_json::json!({
                                "id": "synthetic", "model": body["model"],
                                "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok", "reasoning_content": "synthetic reasoning"}, "finish_reason": "stop"}],
                                "usage": usage
                            }).to_string()
                        };
                        let content_type = if streaming {"text/event-stream"} else {"application/json"};
                        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
                    });
                    install_openai_stub_provider(provider, server.addr());
                    crate::llm_config::set_runtime_provider_endpoint_overrides(
                        crate::llm_config::RuntimeProviderEndpointOverrides::single(
                            provider, format!("http://{}/v1", server.addr())
                        ).unwrap(),
                    );
                    let opts = super::super::LlmCallOptions {
                        provider: provider.into(), model: model.into(),
                        messages: vec![serde_json::json!({"role":"user","content":"Say ok"})],
                        max_tokens: 64, stream: streaming,
                        ..Default::default()
                    };
                    let temp = tempfile::tempdir().unwrap();
                    let admitted = quota(&temp.path().join("spend.sqlite"), 2_000_000);
                    let result = admitted.scope(vm_call_llm_full(&opts)).await.unwrap().unwrap();
                    let receipt = admitted.receipt().unwrap();
                    if complete_usage {
                        assert_eq!(result.output_tokens, 12, "reasoning is already in completion total");
                        assert_eq!(result.telemetry.server_output_tokens, Some(12));
                        assert!(receipt.lifetime_actual_known_microusd > 0);
                        // The durable receipt retains the settled charge in
                        // `reserved`; it releases only the unused upper bound.
                        assert!(receipt.lifetime_reserved_microusd >= receipt.lifetime_actual_known_microusd);
                        assert!(receipt.lifetime_reserved_microusd < 1_000);
                        assert_eq!(receipt.lifetime_remaining_microusd, Some(2_000_000 - receipt.lifetime_reserved_microusd));
                        assert_eq!(receipt.lifetime_usage_unknown_attempts, 0);
                    } else {
                        assert!(receipt.lifetime_reserved_microusd > 0);
                        assert_eq!(receipt.lifetime_actual_known_microusd, 0);
                        assert_eq!(receipt.lifetime_usage_unknown_attempts, 1);
                    }
                    assert!(!receipt.contract_broken);
                }
            }
        }
    });
}
