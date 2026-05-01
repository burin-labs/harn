use std::sync::{Arc, Mutex};
use std::thread;

use axum::body::Bytes;
use axum::extract::State;
use axum::routing::post;
use axum::Router;
use reqwest::header::HeaderMap;
use reqwest::StatusCode;
use serde_json::Value as JsonValue;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use super::support::*;

#[derive(Clone, Debug)]
struct OtlpRequest {
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

#[derive(Clone)]
struct MockOtelCollectorState {
    requests: Arc<Mutex<Vec<OtlpRequest>>>,
}

struct MockOtelCollector {
    url: String,
    requests: Arc<Mutex<Vec<OtlpRequest>>>,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl MockOtelCollector {
    fn start() -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = MockOtelCollectorState {
            requests: requests.clone(),
        };
        let (url_tx, url_rx) = mpsc::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let app = Router::new()
                    .route("/v1/traces", post(record_otlp_traces))
                    .with_state(state);
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                url_tx.send(format!("http://{addr}")).unwrap();
                axum::serve(listener, app)
                    .with_graceful_shutdown(async {
                        let _ = shutdown_rx.await;
                    })
                    .await
                    .unwrap();
            });
        });

        Self {
            url: url_rx.recv().unwrap(),
            requests,
            shutdown: Some(shutdown_tx),
            thread: Some(thread),
        }
    }

    fn collected_spans(&self) -> Vec<CollectedSpan> {
        let requests = self.requests.lock().unwrap().clone();
        requests
            .into_iter()
            .flat_map(|request| {
                serde_json::from_slice::<JsonValue>(&request.body)
                    .map(|body| collect_spans_from_body(&body))
                    .unwrap_or_default()
            })
            .collect()
    }
}

impl Drop for MockOtelCollector {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Debug)]
struct CollectedSpan {
    name: String,
    trace_id: String,
    span_id: String,
    parent_span_id: Option<String>,
    attributes: Vec<(String, JsonValue)>,
}

async fn record_otlp_traces(
    State(state): State<MockOtelCollectorState>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let captured_headers = headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_ascii_lowercase(), value.to_string()))
        })
        .collect::<Vec<_>>();
    state.requests.lock().unwrap().push(OtlpRequest {
        headers: captured_headers,
        body: body.to_vec(),
    });
    StatusCode::OK
}

fn collect_spans_from_body(body: &JsonValue) -> Vec<CollectedSpan> {
    let mut spans = Vec::new();
    let Some(resource_spans) = body.get("resourceSpans").and_then(JsonValue::as_array) else {
        return spans;
    };

    for resource_span in resource_spans {
        let Some(scope_spans) = resource_span
            .get("scopeSpans")
            .and_then(JsonValue::as_array)
        else {
            continue;
        };
        for scope_span in scope_spans {
            let Some(otel_spans) = scope_span.get("spans").and_then(JsonValue::as_array) else {
                continue;
            };
            for span in otel_spans {
                spans.push(CollectedSpan {
                    name: span
                        .get("name")
                        .and_then(JsonValue::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    trace_id: span
                        .get("traceId")
                        .and_then(JsonValue::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    span_id: span
                        .get("spanId")
                        .and_then(JsonValue::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    parent_span_id: span
                        .get("parentSpanId")
                        .and_then(JsonValue::as_str)
                        .map(ToString::to_string)
                        .filter(|value| !value.is_empty()),
                    attributes: span
                        .get("attributes")
                        .and_then(JsonValue::as_array)
                        .map(|attributes| {
                            attributes
                                .iter()
                                .filter_map(|attribute| {
                                    Some((
                                        attribute.get("key")?.as_str()?.to_string(),
                                        attribute.get("value")?.clone(),
                                    ))
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default(),
                });
            }
        }
    }

    spans
}

fn attribute_string(span: &CollectedSpan, key: &str) -> Option<String> {
    span.attributes
        .iter()
        .find(|(name, _)| name == key)
        .and_then(|(_, value)| {
            value
                .get("stringValue")
                .and_then(JsonValue::as_str)
                .map(ToString::to_string)
                .or_else(|| {
                    value
                        .get("intValue")
                        .map(|value| {
                            value
                                .as_str()
                                .map(ToString::to_string)
                                .or_else(|| value.as_i64().map(|value| value.to_string()))
                        })
                        .and_then(|value| value)
                })
        })
}

#[tokio::test(flavor = "multi_thread")]
async fn json_log_format_writes_structured_rotating_file_with_trace_ids() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &base_manifest(None));
    write_file(temp.path(), "lib.harn", handler_module());

    let secret = "json-log-secret";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", secret),
        ("RUST_LOG", "info"),
    ];
    let mut process = spawn_orchestrator(&temp, &["--log-format", "json"], &envs);
    let base_url = process.wait_for_listener_url();

    let body = br#"{"action":"opened","issue":{"number":5}}"#;
    let response = reqwest::Client::new()
        .post(format!("{base_url}/triggers/github-new-issue"))
        .headers(github_headers(secret, body, None))
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::OK).await;

    send_sigterm(&mut process.child);
    let status = wait_for_exit_async(&mut process.child).await;
    let stderr = process.join_stderr();
    assert!(status.success(), "status={status} stderr={stderr}");

    let log_path = temp.path().join("state/logs/orchestrator.log");
    let log = fs::read_to_string(&log_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", log_path.display()));
    let records: Vec<JsonValue> = log
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap_or_else(|error| panic!("{error}: {line}")))
        .collect();
    assert!(
        records.iter().any(|record| record
            .get("message")
            .and_then(JsonValue::as_str)
            .is_some_and(|message| message == "trigger event accepted")),
        "log={log}"
    );
    assert!(
        records.iter().all(|record| record.get("trace_id").is_some()
            || record
                .get("fields")
                .and_then(|fields| fields.get("trace_id"))
                .is_some()),
        "log={log}"
    );
}

// Regression coverage for harn#327 and harn#479: ingest should inject W3C
// trace-context headers, queue append should preserve the trace, and dispatch
// should adopt the queue append span as its remote parent.
//
// The orchestrator subprocess runs with the simple span processor
// (`HARN_OTEL_SPAN_PROCESSOR=simple`). That replaces the production batch
// pipeline with a synchronous "export each span on close" pipeline. The test
// waits for the existing pump lifecycle event before shutdown, so it asserts
// trace propagation after the dispatch span has actually closed instead of
// racing the ack-first inbox pump.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn otel_exports_ingest_and_dispatch_spans_with_shared_trace_id() {
    let _lock = lock_orchestrator_tests();
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &base_manifest(None));
    write_file(temp.path(), "lib.harn", handler_module());

    let collector = MockOtelCollector::start();
    let secret = "otel-secret";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", secret),
        ("HARN_OTEL_ENDPOINT", collector.url.as_str()),
        ("HARN_OTEL_SERVICE_NAME", "harn-orchestrator-test"),
        (
            "HARN_OTEL_HEADERS",
            "authorization=Bearer otel-token,x-tenant-id=tenant-abc",
        ),
        // Synchronous export per span close. See doc comment above this fn.
        ("HARN_OTEL_SPAN_PROCESSOR", "simple"),
        ("RUST_LOG", "info"),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let body = br#"{"action":"opened","issue":{"number":5}}"#;
    let client = reqwest::Client::new();
    let request = client
        .post(format!("{base_url}/triggers/github-new-issue"))
        .headers(github_headers(secret, body, None))
        .body(body.to_vec())
        .build()
        .unwrap();
    let response = client.execute(request).await.unwrap();
    assert_status(response, StatusCode::OK).await;

    wait_for_topic_event(&temp, "orchestrator.lifecycle", |event| {
        event.kind == "pump_dispatch_completed"
            && event.payload["status"] == serde_json::json!("completed")
    })
    .await;

    send_sigterm(&mut process.child);
    let status = wait_for_exit_async(&mut process.child).await;
    let stderr = process.join_stderr();
    assert!(status.success(), "status={status} stderr={stderr}");
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");

    let deadline = Instant::now() + EVENT_FAIL_FAST_TIMEOUT;
    let spans = loop {
        let spans = collector.collected_spans();
        let has_ingest = spans.iter().any(|span| span.name == "ingest");
        let has_queue_append = spans.iter().any(|span| span.name == "queue_append");
        let has_dispatch = spans.iter().any(|span| span.name == "dispatch");
        if has_ingest && has_queue_append && has_dispatch {
            break spans;
        }
        if Instant::now() >= deadline {
            let requests = collector.requests.lock().unwrap().clone();
            panic!(
                "timed out waiting for OTel spans\ncollector_headers={:#?}",
                requests
                    .iter()
                    .map(|request| request.headers.clone())
                    .collect::<Vec<_>>()
            );
        }
        timing::sleep_async(timing::RETRY_POLL_INTERVAL).await;
    };

    let ingest = spans.iter().find(|span| span.name == "ingest").unwrap();
    let queue_append = spans
        .iter()
        .find(|span| span.name == "queue_append")
        .unwrap();
    let dispatch = spans.iter().find(|span| span.name == "dispatch").unwrap();
    assert_eq!(ingest.trace_id, queue_append.trace_id);
    assert_eq!(ingest.trace_id, dispatch.trace_id);
    assert_ne!(ingest.span_id, queue_append.span_id);
    assert_ne!(ingest.span_id, dispatch.span_id);
    assert!(queue_append.parent_span_id.is_some());
    assert_eq!(
        dispatch.parent_span_id.as_deref(),
        Some(queue_append.span_id.as_str())
    );

    let ingest_trace_id = attribute_string(ingest, "trace_id").unwrap();
    let dispatch_trace_id = attribute_string(dispatch, "trace_id").unwrap();
    assert_eq!(ingest_trace_id, dispatch_trace_id);
    assert_eq!(
        attribute_string(dispatch, "result.status").as_deref(),
        Some("succeeded")
    );
    assert!(
        attribute_string(dispatch, "result.duration_ms").is_some(),
        "dispatch span was missing duration attribute: {dispatch:?}"
    );

    let requests = collector.requests.lock().unwrap().clone();
    assert!(
        requests.iter().any(|request| {
            request
                .headers
                .iter()
                .any(|(name, value)| name == "authorization" && value == "Bearer otel-token")
        }),
        "collector never saw Authorization header: {requests:?}"
    );
    assert!(
        requests.iter().any(|request| {
            request
                .headers
                .iter()
                .any(|(name, value)| name == "x-tenant-id" && value == "tenant-abc")
        }),
        "collector never saw tenant header: {requests:?}"
    );
}
