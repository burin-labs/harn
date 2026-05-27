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

#![cfg(feature = "otel")]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use harn_vm::events::{
    add_event_sink, clear_event_sinks, emit_span_end, emit_span_start, install_otel_sink_from_env,
    reset_event_sinks, shutdown_otel_sink, CollectorSink,
};

/// Read the full HTTP request (headers + chunked body) into memory.
/// The opentelemetry-otlp HTTP client uses `Content-Length`, never
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
                    // Try to detect content-length and keep reading.
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_otel_sink_emits_spans_to_configured_endpoint() {
    // Stand up the single-shot HTTP listener first so the port is
    // bound before we point the exporter at it.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind otlp stub");
    listener.set_nonblocking(false).expect("blocking otlp stub");
    let addr = listener.local_addr().expect("stub addr");

    let received = Arc::new(Mutex::new(Vec::<String>::new()));
    let received_clone = received.clone();
    let stub = thread::spawn(move || {
        // Batch flushes can issue more than one request when the
        // exporter retries internally. Accept until the test thread
        // closes the listener — the test does this implicitly when it
        // drops the JoinHandle.
        while let Ok((mut stream, _)) = listener.accept() {
            let request = read_request(&mut stream);
            received_clone
                .lock()
                .expect("stub mutex poisoned")
                .push(request);
            // Minimal OTLP/HTTP success response — the body is an
            // empty `ExportTraceServiceResponse` so the client doesn't
            // mark the export as a failure and trigger a retry storm.
            let response =
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}";
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    });

    // SAFETY: the test process is single-threaded by construction
    // because the integration-test binary only contains this one
    // test. No other consumer is reading these vars while we run.
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

    // Emit one span pair. The first call to `install` registered the
    // batch processor; emit/end go through the EventSink chain to the
    // sink we just installed.
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

    // Reset the sink chain after shutdown so the OtelSink's own
    // `Drop` is the no-op path (the provider is already shut down).
    reset_event_sinks();

    // Give the listener thread up to 5 s to receive the request. In
    // practice the shutdown-driven flush completes well inside 50 ms;
    // the budget guards against macOS scheduling jitter under nextest
    // flake-detection profiles. The yield loop polls every 25 ms so
    // we exit as soon as data arrives.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if !received.lock().expect("stub mutex poisoned").is_empty() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "OTLP stub never received an export within 5s",
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let snapshot = received.lock().expect("stub mutex poisoned").clone();
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

    // Drop the listener-owning thread by letting it fall out of scope.
    drop(stub);

    // SAFETY: see above — single-threaded test process.
    unsafe {
        std::env::remove_var("HARN_OTEL_ENDPOINT");
        std::env::remove_var("HARN_OTEL_SERVICE_NAME");
    }
}
