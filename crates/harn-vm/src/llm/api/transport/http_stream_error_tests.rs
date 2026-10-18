use super::capture::RawProviderCaptureTarget;
use super::liveness::StreamDeadlinePolicy;
use super::sse::vm_call_llm_api_sse_from_response;
use crate::llm::api::DialectContract;
use crate::llm::capabilities::WireDialect;
use crate::value::{ProviderStreamDeadline, ProviderStreamFailureReason, ProviderStreamPhase};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const PARTIAL: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"observed\"}}]}\n\n";

async fn response(
    initial: &'static str,
    end: Option<&'static str>,
) -> (
    reqwest::Response,
    tokio::task::JoinHandle<()>,
    tokio::sync::oneshot::Sender<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind HTTP fixture");
    let address = listener.local_addr().expect("fixture address");
    let (release, released) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept HTTP request");
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(socket.read_u8().await.expect("read HTTP request"));
        }
        socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n").await.expect("HTTP headers");
        if !initial.is_empty() {
            socket
                .write_all(format!("{:x}\r\n{initial}\r\n", initial.len()).as_bytes())
                .await
                .expect("HTTP body chunk");
        }
        if let Some(end) = end {
            released.await.expect("release HTTP ending");
            socket.write_all(end.as_bytes()).await.expect("HTTP ending");
        } else {
            std::future::pending::<()>().await;
        }
    });
    let response = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_mins(1))
        .build()
        .expect("HTTP client")
        .get(format!("http://{address}/stream"))
        .send()
        .await
        .expect("real HTTP response headers");
    (response, server, release)
}

async fn consume(
    response: reqwest::Response,
    policy: StreamDeadlinePolicy,
    delta_tx: super::DeltaSender,
) -> Result<crate::llm::api::LlmResult, crate::value::VmError> {
    vm_call_llm_api_sse_from_response(
        response,
        "fixture",
        "test-model",
        DialectContract::new(WireDialect::OpenAiCompat, None),
        delta_tx,
        None,
        None,
        false,
        policy,
        RawProviderCaptureTarget::new(None, None),
        None,
        tokio::time::Instant::now(),
    )
    .await
}

fn policy(total: u64, first: u64, idle: u64) -> StreamDeadlinePolicy {
    StreamDeadlinePolicy::for_test(
        Duration::from_secs(total),
        Duration::from_secs(first),
        Duration::from_secs(idle),
    )
}

async fn observe_partial(rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>) {
    let delta = rx.recv().await.expect("received provider text");
    assert!(!delta.is_empty() && "observed".starts_with(&delta));
}

#[tokio::test]
async fn real_http_body_timeout_retains_total_deadline_after_partial_output() {
    let (response, server, _release) = response(PARTIAL, None).await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    // The HTTP client's deadline must fire before the parser's own timer.
    let reader = tokio::spawn(consume(response, policy(120, 120, 120), tx));
    observe_partial(&mut rx).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_mins(1)).await;
    let error = reader
        .await
        .expect("reader task")
        .expect_err("HTTP timeout");
    server.abort();
    let failure = error.provider_stream_failure().expect("typed failure");
    assert_eq!(failure.reason, ProviderStreamFailureReason::Deadline);
    assert_eq!(failure.deadline, Some(ProviderStreamDeadline::Total));
    assert_eq!(failure.phase, ProviderStreamPhase::Streaming);
    assert!(failure.partial);
}

#[tokio::test]
async fn real_http_ndjson_body_timeout_retains_total_deadline() {
    let (response, server, _release) = response(
        "{\"message\":{\"content\":\"observed\"},\"done\":false}\n",
        None,
    )
    .await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let reader = tokio::spawn(async move {
        let mut warmup_gate = false;
        super::ndjson::vm_call_llm_api_ndjson_from_response(
            response,
            "ollama",
            "test-model",
            tx,
            Duration::ZERO,
            &mut warmup_gate,
            None,
            policy(120, 120, 120),
            RawProviderCaptureTarget::new(None, None),
            tokio::time::Instant::now(),
        )
        .await
    });
    observe_partial(&mut rx).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_mins(1)).await;
    let error = reader
        .await
        .expect("reader task")
        .expect_err("HTTP timeout");
    server.abort();
    let failure = error.provider_stream_failure().expect("typed failure");
    assert_eq!(failure.reason, ProviderStreamFailureReason::Deadline);
    assert_eq!(failure.deadline, Some(ProviderStreamDeadline::Total));
    assert!(failure.partial);
}

#[tokio::test]
async fn real_http_first_chunk_and_idle_deadlines_remain_distinct() {
    for (initial, expected) in [
        ("", ProviderStreamDeadline::FirstChunk),
        (PARTIAL, ProviderStreamDeadline::Idle),
    ] {
        let (response, server, _release) = response(initial, None).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let reader = tokio::spawn(consume(response, policy(120, 5, 5), tx));
        if !initial.is_empty() {
            observe_partial(&mut rx).await;
        }
        tokio::time::pause();
        let error = reader.await.expect("reader task").expect_err("deadline");
        server.abort();
        let failure = error.provider_stream_failure().expect("typed failure");
        assert_eq!(failure.reason, ProviderStreamFailureReason::Deadline);
        assert_eq!(failure.deadline, Some(expected));
        assert_eq!(failure.partial, !initial.is_empty());
        assert_eq!(
            failure.phase,
            if initial.is_empty() {
                ProviderStreamPhase::AwaitingFirstChunk
            } else {
                ProviderStreamPhase::Streaming
            }
        );
        tokio::time::resume();
    }
}

#[tokio::test]
async fn real_http_body_errors_and_clean_premature_eof_are_not_deadlines() {
    for (ending, expected) in [
        ("invalid-chunk-size\r\n", ProviderStreamFailureReason::Read),
        ("0\r\n\r\n", ProviderStreamFailureReason::PrematureEof),
    ] {
        let (response, server, release) = response(PARTIAL, Some(ending)).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let reader = tokio::spawn(consume(response, policy(120, 120, 120), tx));
        observe_partial(&mut rx).await;
        release.send(()).expect("release HTTP ending");
        let error = reader
            .await
            .expect("reader task")
            .expect_err("incomplete stream");
        server.await.expect("fixture task");
        let failure = error.provider_stream_failure().expect("typed failure");
        assert_eq!(failure.reason, expected);
        assert_eq!(failure.deadline, None);
        assert!(failure.partial);
    }
}

#[tokio::test]
async fn real_http_terminal_frame_completes() {
    let (response, server, release) = response(
        "data: {\"choices\":[{\"delta\":{\"content\":\"complete\"},\"finish_reason\":\"stop\"}]}\n\n",
        Some("0\r\n\r\n"),
    )
    .await;
    release.send(()).expect("release HTTP ending");
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let result = consume(response, policy(120, 120, 120), tx)
        .await
        .expect("completed HTTP stream");
    server.await.expect("fixture task");
    assert_eq!(result.text, "complete");
}
