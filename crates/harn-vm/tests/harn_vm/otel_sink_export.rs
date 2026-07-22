//! End-to-end check that the local OTLP exporter actually delivers
//! Harn spans to a configured collector once the env var is set.
//!
//! The test stands up a single-shot raw HTTP listener, points
//! `HARN_OTEL_ENDPOINT` at it, exercises [`emit_span_start`] /
//! [`emit_span_end`], and then drops the sink to force the batch
//! processor to flush. The captured request body is asserted to be a
//! `POST /v1/traces` containing the emitted span name, providing
//! confidence that the wiring between `install_otel_sink_from_env`
//! and the `opentelemetry-otlp` exporter is intact.
//!
//! The former file-level `#![cfg(feature = "otel")]` gate now lives on the
//! `mod otel_sink_export;` declaration in the `harn_vm` test root.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

use harn_vm::events::{
    add_event_sink, clear_event_sinks, emit_span_end, emit_span_start, install_otel_sink_from_env,
    reset_event_sinks, shutdown_otel_sink, CollectorSink,
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::timeout;

/// Read the full HTTP request (headers + body) into memory.
/// `opentelemetry-otlp`'s HTTP client uses `Content-Length`, never
/// chunked transfer, so this short read loop is enough.
fn read_request(stream: &mut std::net::TcpStream) -> String {
    let mut buf = vec![0u8; 16 * 1024];
    let mut text = String::new();
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                text.push_str(&String::from_utf8_lossy(&buf[..n]));
                if text.contains("\r\n\r\n") {
                    if let Some(cl) = content_length(&text) {
                        let header_end = text.find("\r\n\r\n").unwrap() + 4;
                        if text.len() - header_end >= cl {
                            break;
                        }
                    } else {
                        break;
                    }
                }
            }
            Err(_) => break,
        }
    }
    text
}

fn content_length(headers: &str) -> Option<usize> {
    for line in headers.lines() {
        if let Some(value) = line
            .split_once(':')
            .filter(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
            .map(|(_, value)| value.trim())
        {
            return value.parse().ok();
        }
    }
    None
}

/// Drain any additional already-buffered requests after the first one
/// arrives. Returns the snapshot of all requests received so far.
async fn drain_remaining(rx: &mut UnboundedReceiver<String>, head: String) -> Vec<String> {
    let mut out = vec![head];
    while let Ok(Some(msg)) = timeout(Duration::from_millis(10), rx.recv()).await {
        out.push(msg);
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_otel_sink_emits_spans_to_configured_endpoint() {
    // Bind the OTLP stub before we point the exporter at it.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind otlp stub");
    listener.set_nonblocking(false).expect("blocking otlp stub");
    let addr = listener.local_addr().expect("stub addr");

    // Sync → async bridge: the stub thread pushes received requests
    // into a tokio mpsc channel the async test awaits on. Avoids
    // polling the shared `Vec` with wall-clock sleep + Instant
    // comparisons — both of which the test-pattern audit forbids
    // because they degrade into flakes under runner contention.
    let (tx, mut rx): (UnboundedSender<String>, UnboundedReceiver<String>) =
        mpsc::unbounded_channel();
    let stub = thread::spawn(move || {
        // Batch flushes can issue more than one request when the
        // exporter retries internally. Accept until the listener is
        // dropped — the test does this implicitly when it returns.
        while let Ok((mut stream, _)) = listener.accept() {
            let request = read_request(&mut stream);
            // Test side may already have torn down the channel; we
            // don't care, the listener exits cleanly either way.
            let _ = tx.send(request);
            // Minimal OTLP/HTTP success: empty
            // `ExportTraceServiceResponse` so the client doesn't
            // mark the export as a failure and retry.
            let response =
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}";
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    });

    // SAFETY: the integration-test binary contains only this one
    // test, so no other consumer is reading these vars while we run.
    unsafe {
        std::env::set_var("HARN_OTEL_ENDPOINT", format!("http://{addr}"));
        std::env::set_var("HARN_OTEL_SERVICE_NAME", "burin-otel-smoke");
        std::env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT");
    }

    // Start from an empty sink chain so we can also assert that the
    // helper added exactly one sink.
    clear_event_sinks();
    let probe = std::rc::Rc::new(CollectorSink::new());
    add_event_sink(probe.clone());

    let installed =
        install_otel_sink_from_env().expect("otel sink install failed against the loopback stub");
    assert!(installed, "expected sink to install when endpoint is set");

    let mut start_meta = BTreeMap::new();
    start_meta.insert("turn".to_string(), serde_json::json!(7));
    emit_span_start(42, None, "burin.turn", "agent_loop", start_meta);
    emit_span_end(42, BTreeMap::new());

    // Sanity: our CollectorSink also received the event. This proves
    // additional sinks coexist with the OtelSink (regression cover
    // for accidentally clobbering the chain).
    assert_eq!(probe.spans.borrow().len(), 1);
    assert_eq!(probe.spans.borrow()[0].name, "burin.turn");

    // Drain the batch explicitly via the public shutdown hook —
    // mirrors what `harn-cli` does on process exit. Also clears the
    // global provider slot so a re-run inside the same process can
    // call `install_otel_sink_from_env` again.
    let was_shut = shutdown_otel_sink().expect("shutdown_otel_sink errored");
    assert!(was_shut, "expected shutdown to flush an installed provider");

    reset_event_sinks();

    // Wait for the first request through the channel. The 5 s
    // budget guards against macOS scheduling jitter under
    // flake-detection profiles; the typical path completes in under
    // 50 ms because `shutdown_otel_sink` blocks on `force_flush`.
    let first = timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("OTLP stub never received an export within 5s")
        .expect("OTLP stub channel closed unexpectedly");
    let snapshot = drain_remaining(&mut rx, first).await;

    let combined = snapshot.join("\n----\n");
    assert!(
        combined.contains("POST /v1/traces"),
        "expected a POST to /v1/traces, got:\n{combined}",
    );
    assert!(
        combined.contains("burin.turn"),
        "expected the OTLP payload to carry the emitted span name, got:\n{combined}",
    );
    assert!(
        combined.contains("burin-otel-smoke"),
        "expected the service.name resource attribute in payload, got:\n{combined}",
    );

    drop(stub);

    // SAFETY: see above — single-threaded test process.
    unsafe {
        std::env::remove_var("HARN_OTEL_ENDPOINT");
        std::env::remove_var("HARN_OTEL_SERVICE_NAME");
    }
}
