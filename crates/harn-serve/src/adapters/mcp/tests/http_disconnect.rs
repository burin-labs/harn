//! The response stream owns a stable direct call, but never an MCP task.
use super::*;
use crate::adapters::mcp::transport::{http_post_request, spawn_http_stream_with_prelude};

pub(super) struct GateConfigurator {
    pub(super) events: mpsc::UnboundedSender<(String, String)>,
    pub(super) progress: mpsc::UnboundedSender<harn_vm::mcp_progress::ProgressContext>,
    pub(super) releases: Arc<HashMap<String, Arc<tokio::sync::Semaphore>>>,
    pub(super) tokens: Option<mpsc::UnboundedSender<(String, Arc<AtomicBool>)>>,
}

impl crate::VmConfigurator for GateConfigurator {
    fn configure(&self, vm: &mut harn_vm::Vm) -> Result<(), DispatchError> {
        let events = self.events.clone();
        let releases = self.releases.clone();
        let progress = self.progress.clone();
        let tokens = self.tokens.clone();
        vm.register_async_builtin("test_gate", move |ctx, args| {
            let value = args[0].display();
            let cancellation = ctx
                .child_vm()
                .cancellation_token()
                .expect("dispatch cancellation token");
            if let Some(tokens) = &tokens {
                tokens
                    .send((value.clone(), cancellation.clone()))
                    .expect("token reader");
            }
            let release = releases.get(&value).expect("registered gate").clone();
            let events = events.clone();
            let progress = progress.clone();
            async move {
                if let Some(context) = harn_vm::mcp_progress::current_context() {
                    progress.send(context).expect("progress reader");
                }
                events
                    .send(("entered".into(), value.clone()))
                    .expect("event reader");
                release.acquire().await.expect("gate stays open").forget();
                if cancellation.load(Ordering::SeqCst) {
                    events
                        .send(("stopped".into(), value))
                        .expect("event reader");
                    return Err(harn_vm::VmError::CategorizedError {
                        message: "fixture host operation cancelled".into(),
                        category: harn_vm::ErrorCategory::Cancelled,
                    });
                }
                Ok(harn_vm::VmValue::Nil)
            }
        });
        let events = self.events.clone();
        vm.register_builtin("test_mark", move |args, _| {
            events
                .send((args[0].display(), args[1].display()))
                .expect("event reader");
            Ok(harn_vm::VmValue::Nil)
        });
        Ok(())
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    server: Arc<McpServer>,
    events: mpsc::UnboundedReceiver<(String, String)>,
    progress: mpsc::UnboundedReceiver<harn_vm::mcp_progress::ProgressContext>,
    releases: Arc<HashMap<String, Arc<tokio::sync::Semaphore>>>,
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().expect("fixture directory");
    let script = directory.path().join("server.harn");
    std::fs::write(
        &script,
        r#"
fn gate(value: string) -> string {
  test_gate(value)
  test_mark("continued", value)
  return value
}
@annotations(readOnly: true, idempotent: true)
pub fn direct(value: string) -> string {
  return gate(value)
}
@job("independent")
pub fn task(value: string) -> string {
  return gate(value)
}
"#,
    )
    .expect("write fixture");
    let releases = Arc::new(HashMap::from([
        ("cancel".into(), Arc::new(tokio::sync::Semaphore::new(0))),
        ("other".into(), Arc::new(tokio::sync::Semaphore::new(0))),
        ("task".into(), Arc::new(tokio::sync::Semaphore::new(0))),
    ]));
    let (events, receiver) = mpsc::unbounded_channel();
    let (progress, progress_receiver) = mpsc::unbounded_channel();
    let mut config = DispatchCoreConfig::for_script(&script);
    config.max_dispatch_workers = NonZeroUsize::new(2).expect("two workers");
    config.vm_configurator = Arc::new(GateConfigurator {
        events,
        progress,
        releases: releases.clone(),
        tokens: None,
    });
    Fixture {
        _directory: directory,
        server: Arc::new(McpServer::new(McpServerConfig::new(
            DispatchCore::new(config).expect("dispatch core"),
        ))),
        events: receiver,
        progress: progress_receiver,
        releases,
    }
}

fn request(tool: &str, value: &str) -> JsonValue {
    stable_request(harn_vm::jsonrpc::request(
        7,
        "tools/call",
        json!({"name": tool, "arguments": {"value": value}}),
    ))
}

pub(super) async fn http_call(
    server: Arc<McpServer>,
    value: &str,
    accept: &'static str,
) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static(accept));
    headers.insert("mcp-method", HeaderValue::from_static("tools/call"));
    headers.insert("mcp-name", HeaderValue::from_static("direct"));
    headers.insert(
        "mcp-protocol-version",
        HeaderValue::from_static(MCP_PROTOCOL_VERSION),
    );
    let response = http_post_request(
        State(HttpState {
            server,
            options: McpHttpServeOptions::default(),
        }),
        Method::POST,
        headers,
        Bytes::from(serde_json::to_vec(&request("direct", value)).expect("request JSON")),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], accept);
    response
}

#[tokio::test]
async fn dropping_http_response_stops_reached_harn_handler_and_isolates_other_request() {
    let mut fixture = fixture();
    // Reusing the JSON-RPC id is deliberate: each HTTP request has its own
    // SharedSession, so closing one must not cancel the other.
    let cancelled = http_call(fixture.server.clone(), "cancel", "text/event-stream").await;
    let other = http_call(fixture.server.clone(), "other", "text/event-stream").await;
    let first = fixture.events.recv().await.expect("first handler entered");
    let second = fixture.events.recv().await.expect("second handler entered");
    assert_eq!(first.0, "entered");
    assert_eq!(second.0, "entered");
    assert_ne!(first.1, second.1);
    drop(cancelled.into_body());
    fixture.releases["cancel"].add_permits(1);
    fixture.releases["other"].add_permits(1);
    let first = fixture.events.recv().await.expect("first terminal witness");
    let second = fixture
        .events
        .recv()
        .await
        .expect("second terminal witness");
    assert_eq!(
        std::collections::BTreeSet::from([first, second]),
        std::collections::BTreeSet::from([
            ("stopped".into(), "cancel".into()),
            ("continued".into(), "other".into()),
        ])
    );
    let body = axum::body::to_bytes(other.into_body(), 64 * 1024)
        .await
        .expect("other response completes");
    let body = String::from_utf8(body.to_vec()).expect("SSE UTF-8");
    assert!(body.contains("\"id\":7"), "{body}");
    assert!(body.contains("other"), "{body}");
    let extra = fixture.events.try_recv();
    assert!(extra.is_err(), "unexpected handler event: {extra:?}");
}

#[tokio::test]
async fn dropping_json_http_exchange_stops_the_dispatched_harn_handler() {
    let mut fixture = fixture();
    let server = fixture.server.clone();
    let exchange =
        tokio::spawn(async move { http_call(server, "cancel", "application/json").await });
    assert_eq!(
        fixture.events.recv().await,
        Some(("entered".into(), "cancel".into()))
    );
    exchange.abort();
    assert!(exchange
        .await
        .expect_err("HTTP exchange was dropped")
        .is_cancelled());
    fixture.releases["cancel"].add_permits(1);
    assert_eq!(
        fixture.events.recv().await,
        Some(("stopped".into(), "cancel".into()))
    );
    assert!(
        fixture.events.try_recv().is_err(),
        "handler continued after HTTP cancellation"
    );
}

#[tokio::test]
async fn disconnect_before_registration_prevents_handler_invocation() {
    let fixture = fixture();
    let session = SharedSession::new();
    let job = match fixture
        .server
        .process_message(
            request("direct", "cancel"),
            session.clone(),
            AuthRequest::default(),
        )
        .await
    {
        ImmediateResult::Stream(job) => job,
        _ => panic!("direct stream job"),
    };
    let active = job.active.clone();
    assert!(!active.cancel_token.load(Ordering::SeqCst));
    // On a current-thread runtime the spawned execution cannot register until
    // this task yields. The receiver is closed first, through the real SSE body.
    let response = transport::spawn_http_stream(fixture.server.clone(), *job).into_response();
    drop(response.into_body());
    assert!(active.cancel_token.load(Ordering::SeqCst));
    assert!(active.cancelled.load(Ordering::SeqCst));
    // The same prepared state must refuse invocation when execution is polled.
    // Await a second prepared, already-cancelled job to prove that admission
    // without relying on scheduling or an empty event census alone.
    let mut job = match fixture
        .server
        .process_message(
            request("direct", "cancel"),
            session.clone(),
            AuthRequest::default(),
        )
        .await
    {
        ImmediateResult::Stream(job) => job,
        _ => panic!("direct stream job"),
    };
    job.active = active;
    fixture.server.execute_streaming_job(*job, discard()).await;
    assert!(session
        .inner
        .lock()
        .expect("session")
        .active_calls
        .is_empty());
    assert!(fixture.events.is_empty());
}

#[tokio::test]
async fn closing_task_stream_preserves_its_handler_and_terminal_result() {
    let mut fixture = fixture();
    let session = SharedSession::new();
    let (immediate, job) = match fixture
        .server
        .process_message(
            task_client_request(request("task", "task")),
            session.clone(),
            AuthRequest::default(),
        )
        .await
    {
        ImmediateResult::TaskStream { immediate, job } => (immediate, job),
        _ => panic!("task stream job"),
    };
    let task_id = immediate["result"]["taskId"]
        .as_str()
        .expect("task id")
        .to_owned();
    let active = job.active.clone();
    let access = harn_vm::mcp_tasks::McpTaskAccess::unscoped();
    let terminal = fixture
        .server
        .tasks
        .notifier(&access, &task_id)
        .expect("known task");
    let completed = terminal.notified();
    tokio::pin!(completed);
    completed.as_mut().enable();
    let response =
        spawn_http_stream_with_prelude(fixture.server.clone(), *job, immediate).into_response();
    assert_eq!(
        fixture.events.recv().await,
        Some(("entered".into(), "task".into()))
    );
    drop(response.into_body());
    assert!(!active.cancel_token.load(Ordering::SeqCst));
    fixture.releases["task"].add_permits(1);
    assert_eq!(
        fixture.events.recv().await,
        Some(("continued".into(), "task".into()))
    );
    completed.await;
    let result = fixture
        .server
        .tasks
        .handle_get(&access, json!(8), &json!({"taskId": task_id}));
    assert_eq!(result["result"]["status"], "completed");
    assert!(fixture.events.try_recv().is_err());
}

#[tokio::test]
async fn cancellation_suppresses_reached_progress_and_terminal_writes() {
    let mut fixture = fixture();
    let session = SharedSession::new();
    let mut request = request("direct", "cancel");
    request["params"]["_meta"]["progressToken"] = json!(1);
    let job = match fixture
        .server
        .process_message(request, session.clone(), AuthRequest::default())
        .await
    {
        ImmediateResult::Stream(job) => job,
        _ => panic!("direct stream job"),
    };
    let (messages, mut received) = mpsc::unbounded_channel();
    let server = fixture.server.clone();
    let execution = tokio::spawn(async move {
        server
            .execute_streaming_job(
                *job,
                notify_channel(move |message| {
                    messages.send(message).expect("message reader");
                }),
            )
            .await;
    });
    assert_eq!(
        fixture.events.recv().await,
        Some(("entered".into(), "cancel".into()))
    );
    let progress = fixture
        .progress
        .recv()
        .await
        .expect("actual handler progress context");
    assert!(progress.report(1.0, Some(3.0), None));
    assert_eq!(
        received.recv().await.expect("positive progress")["method"],
        "notifications/progress"
    );
    fixture
        .server
        .handle_cancel_notification(&session, &json!({"requestId": 7}));
    assert!(progress.report(2.0, Some(3.0), None));
    fixture.releases["cancel"].add_permits(1);
    execution.await.expect("cancelled execution stops");
    assert_eq!(
        fixture.events.recv().await,
        Some(("stopped".into(), "cancel".into()))
    );
    assert!(
        received.try_recv().is_err(),
        "cancelled progress or terminal response escaped"
    );
}
