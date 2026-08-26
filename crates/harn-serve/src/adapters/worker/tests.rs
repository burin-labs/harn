use super::test_support::{write_script, ScopedEnvVar, ENV_LOCK};
use super::*;

async fn wait_for_log_event(
    event_log: Arc<AnyEventLog>,
    topic_name: &str,
    matches: impl Fn(&harn_vm::event_log::LogEvent) -> bool,
) -> harn_vm::event_log::LogEvent {
    let topic = Topic::new(topic_name).expect("test topic is valid");
    let latest = event_log.latest(&topic).await.expect("latest event id");
    let mut stream = event_log
        .clone()
        .subscribe(&topic, latest)
        .await
        .expect("subscribe to topic");

    for (_, event) in event_log
        .read_range(&topic, None, usize::MAX)
        .await
        .expect("read topic")
    {
        if matches(&event) {
            return event;
        }
    }

    tokio::time::timeout(StdDuration::from_secs(5), async {
        loop {
            let Some(received) = stream.next().await else {
                panic!("event stream ended before matching event");
            };
            let (_, event) = received.expect("read event");
            if matches(&event) {
                return event;
            }
        }
    })
    .await
    .expect("matching event")
}

async fn wait_for_attempt(
    event_log: Arc<AnyEventLog>,
    trigger_id: &str,
) -> harn_vm::event_log::LogEvent {
    wait_for_log_event(event_log, harn_vm::TRIGGER_ATTEMPTS_TOPIC, |event| {
        event.kind == "attempt_recorded"
            && event
                .payload
                .get("trigger_id")
                .and_then(|value| value.as_str())
                == Some(trigger_id)
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
async fn one_shot_job_echoes_request_and_succeeds() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let script = write_script(
                dir.path(),
                r#"
import "std/triggers"

@job("scan")
pub fn scan(harness: Harness, event: TriggerEvent) -> dict {
  let req = event.provider_payload.raw
  return {status: "ok", echo: req}
}
"#,
            )
            .await;

            let request = serde_json::json!({"repo": "burin-labs/harn", "n": 7});
            let outcome = run_job_once(&script, "scan", request.clone())
                .await
                .expect("run job");

            assert_eq!(outcome.status, DispatchStatus::Succeeded);
            assert_eq!(outcome.attempt_count, 1);
            let result = outcome.result.expect("result");
            assert_eq!(result["status"], serde_json::json!("ok"));
            assert_eq!(result["echo"], request);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn one_shot_resolves_public_job_name_not_function_name() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let script = write_script(
                dir.path(),
                r#"
import "std/triggers"

@job("scan")
pub fn run_scan(harness: Harness, event: TriggerEvent) -> dict {
  return {status: "ok", echo: event.provider_payload.raw}
}
"#,
            )
            .await;

            let outcome = run_job_once(&script, "scan", serde_json::json!({"id": 7}))
                .await
                .expect("run job by public name");

            assert_eq!(outcome.job, "scan");
            assert_eq!(outcome.status, DispatchStatus::Succeeded);
            assert_eq!(
                outcome.result.expect("result")["echo"],
                serde_json::json!({"id": 7})
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn configure_hook_registers_callable_host_builtin() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let script = write_script(
                dir.path(),
                r#"
import "std/triggers"

@job("scan")
pub fn scan(harness: Harness, event: TriggerEvent) -> dict {
  let req = event.provider_payload.raw
  return {status: "ok", host: host_echo(req.repo)}
}
"#,
            )
            .await;

            let request = serde_json::json!({"repo": "burin-labs/harn"});
            let outcome = run_job_once_with(&script, "scan", request, |vm| {
                // An embedder-defined builtin, injected via the configure
                // hook on the fully-built job VM. The `@job` closure calls
                // it by bare name, exactly like the stdlib builtins.
                vm.register_builtin("host_echo", |args, _out| {
                    let x = args.first().map(|a| a.display()).unwrap_or_default();
                    Ok(harn_vm::VmValue::String(arcstr::ArcStr::from(
                        format!("host:{x}").as_str(),
                    )))
                });
            })
            .await
            .expect("run job");

            assert_eq!(outcome.status, DispatchStatus::Succeeded);
            let result = outcome.result.expect("result");
            assert_eq!(result["status"], serde_json::json!("ok"));
            assert_eq!(result["host"], serde_json::json!("host:burin-labs/harn"));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn handler_error_retries_then_dlqs() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let script = write_script(
                dir.path(),
                r#"
import "std/triggers"

@job("scan")
@retry(max: 2, backoff: "linear")
pub fn scan(harness: Harness, event: TriggerEvent) -> dict {
  throw "boom"
}
"#,
            )
            .await;

            let outcome = run_job_once(&script, "scan", serde_json::json!({}))
                .await
                .expect("run job returns terminal outcome");

            assert_eq!(outcome.status, DispatchStatus::Dlq);
            assert_eq!(outcome.attempt_count, 2);
            assert!(!outcome.succeeded());
            // The rendered report is a JSON object even on failure.
            let report = outcome.report_json();
            assert_eq!(report["status"], serde_json::json!("dlq"));
            assert!(report["error"].as_str().is_some_and(|e| e.contains("boom")));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn compact_job_retry_dict_still_maps_to_dispatcher_retry() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let script = write_script(
                dir.path(),
                r#"
import "std/triggers"

@job("scan", retry: { max: 2, backoff: "linear" })
pub fn scan(harness: Harness, event: TriggerEvent) -> dict {
  throw "boom"
}
"#,
            )
            .await;

            let outcome = run_job_once(&script, "scan", serde_json::json!({}))
                .await
                .expect("run job returns terminal outcome");

            assert_eq!(outcome.status, DispatchStatus::Dlq);
            assert_eq!(outcome.attempt_count, 2);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn fail_fast_override_runs_a_single_attempt_for_an_erroring_job() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            // The `@job` declares the production default (svix, max 7),
            // whose backoff would sleep minutes-to-hours between
            // attempts. The driver override must cap it to one attempt.
            let script = write_script(
                dir.path(),
                r#"
import "std/triggers"

@job("scan")
@retry(max: 7, backoff: "svix")
pub fn scan(harness: Harness, event: TriggerEvent) -> dict {
  throw "boom"
}
"#,
            )
            .await;

            let started = std::time::Instant::now();
            let outcome = run_job_once_with_options(
                &script,
                "scan",
                serde_json::json!({}),
                JobRunOptions::fail_fast(),
                |_vm| {},
            )
            .await
            .expect("run job returns terminal outcome");
            let elapsed = started.elapsed();

            // One attempt, no retry, no backoff sleep: terminal failure
            // arrives effectively immediately despite the `@job`'s
            // multi-hour svix policy.
            assert_eq!(outcome.attempt_count, 1);
            assert_eq!(outcome.status, DispatchStatus::Dlq);
            assert!(!outcome.succeeded());
            assert!(
                elapsed < StdDuration::from_secs(5),
                "fail-fast run should not sleep through retry backoff (took {elapsed:?})"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn retry_override_caps_attempts_below_the_job_policy() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            // Declared policy is 5 attempts; the driver caps it to 3 with
            // an immediate (zero-delay) backoff so the test is fast.
            let script = write_script(
                dir.path(),
                r#"
import "std/triggers"

@job("scan")
@retry(max: 5, backoff: "linear")
pub fn scan(harness: Harness, event: TriggerEvent) -> dict {
  throw "boom"
}
"#,
            )
            .await;

            let outcome = run_job_once_with_options(
                &script,
                "scan",
                serde_json::json!({}),
                JobRunOptions::default().with_retry(TriggerRetryConfig::new(
                    3,
                    RetryPolicy::Linear { delay_ms: 0 },
                )),
                |_vm| {},
            )
            .await
            .expect("run job returns terminal outcome");

            assert_eq!(outcome.attempt_count, 3);
            assert_eq!(outcome.status, DispatchStatus::Dlq);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn default_options_preserve_the_job_declared_retry_policy() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            // Linear backoff: attempt 1 is immediate, attempt 2 sleeps
            // 1s, so the default (no override) path stays fast enough to
            // test while still proving the `@job`'s `max: 2` is honoured.
            let script = write_script(
                dir.path(),
                r#"
import "std/triggers"

@job("scan")
@retry(max: 2, backoff: "linear")
pub fn scan(harness: Harness, event: TriggerEvent) -> dict {
  throw "boom"
}
"#,
            )
            .await;

            // `JobRunOptions::default()` carries no override, so the
            // dispatcher must use the `@job`'s declared `max: 2`.
            let outcome = run_job_once_with_options(
                &script,
                "scan",
                serde_json::json!({}),
                JobRunOptions::default(),
                |_vm| {},
            )
            .await
            .expect("run job returns terminal outcome");

            assert_eq!(outcome.attempt_count, 2);
            assert_eq!(outcome.status, DispatchStatus::Dlq);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn worker_server_activates_scheduled_jobs() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let _env_lock = ENV_LOCK.lock().await;
            let _single_tick = ScopedEnvVar::set("HARN_TEST_CRON_SINGLE_TICK_AT", "1700000000");
            let dir = tempfile::tempdir().expect("tempdir");
            let script = write_script(
                dir.path(),
                r#"
import "std/triggers"

@job("tick")
@schedule("* * * * *", "UTC")
pub fn run_tick(harness: Harness, event: TriggerEvent) -> dict {
  return {status: "ok"}
}
"#,
            )
            .await;

            let server = start_worker_server(
                &script,
                WorkerServeOptions {
                    drain_timeout: StdDuration::from_secs(5),
                    ..WorkerServeOptions::default()
                },
            )
            .await
            .expect("start worker server");
            assert_eq!(server.jobs().len(), 1);
            assert_eq!(server.jobs()[0].job, "tick");

            let event_log = server.event_log();
            let attempt = wait_for_attempt(event_log, "job:tick").await;
            assert_eq!(attempt.payload["outcome"], serde_json::json!("success"));

            let report = server.shutdown().await.expect("shutdown worker");
            assert!(report.drained);
            assert_eq!(report.jobs, 1);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn worker_server_consumes_worker_queue_jobs() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let script = write_script(
                dir.path(),
                r#"
import "std/triggers"

@job("scan")
@queue("scan-jobs")
pub fn scan(harness: Harness, event: TriggerEvent) -> dict {
  return {status: "ok", echo: event.provider_payload.raw}
}
"#,
            )
            .await;

            let server = start_worker_server(
                &script,
                WorkerServeOptions {
                    consumer_id: Some("test-worker".to_string()),
                    claim_ttl: StdDuration::from_secs(30),
                    drain_timeout: StdDuration::from_secs(5),
                    ..WorkerServeOptions::default()
                },
            )
            .await
            .expect("start worker server");
            let registration = server.jobs().first().expect("job registration").clone();
            assert_eq!(registration.queue.as_deref(), Some("scan-jobs"));

            let event_log = server.event_log();
            let response_topic = harn_vm::worker_response_topic_name("scan-jobs");
            let topic = Topic::new(response_topic.clone()).expect("response topic");
            let latest = event_log.latest(&topic).await.expect("latest response");
            let mut responses = event_log
                .clone()
                .subscribe(&topic, latest)
                .await
                .expect("subscribe responses");

            let request = serde_json::json!({"repo": "burin-labs/harn"});
            let foreign = WorkerQueue::new(event_log.clone())
                .enqueue(&harn_vm::WorkerQueueJob {
                    queue: "scan-jobs".to_string(),
                    trigger_id: registration.binding_id.clone(),
                    binding_key: registration.binding_key.clone(),
                    binding_version: registration.binding_version,
                    event: job_event(
                        "scan",
                        serde_json::json!({"tenant": "foreign"}),
                        Some(harn_vm::TenantId::new("tenant-a")),
                    ),
                    replay_of_event_id: None,
                    priority: WorkerQueuePriority::Normal,
                })
                .await
                .expect("enqueue tenanted job");
            let event = job_event("scan", request.clone(), None);
            let queue = WorkerQueue::new(event_log.clone());
            let own = queue
                .enqueue(&harn_vm::WorkerQueueJob {
                    queue: "scan-jobs".to_string(),
                    trigger_id: registration.binding_id.clone(),
                    binding_key: registration.binding_key.clone(),
                    binding_version: registration.binding_version,
                    event,
                    replay_of_event_id: None,
                    priority: WorkerQueuePriority::Normal,
                })
                .await
                .expect("enqueue job");

            let response = tokio::time::timeout(StdDuration::from_secs(5), async {
                loop {
                    let Some(received) = responses.next().await else {
                        panic!("response stream ended");
                    };
                    let (_, event) = received.expect("response event");
                    if event.kind != "job_response" {
                        continue;
                    }
                    return serde_json::from_value::<WorkerQueueResponseRecord>(event.payload)
                        .expect("response record");
                }
            })
            .await
            .expect("worker response");

            let outcome = response.outcome.expect("dispatch outcome");
            assert_eq!(response.job_event_id, own.job_event_id);
            assert_eq!(outcome.status, DispatchStatus::Succeeded);
            assert_eq!(outcome.result.expect("result")["echo"], request);
            assert_eq!(response.error, None);
            let state = queue.queue_state("scan-jobs").await.expect("queue state");
            let foreign = state
                .jobs
                .iter()
                .find(|job| job.job_event_id == foreign.job_event_id)
                .expect("foreign job state");
            assert!(!foreign.acked);
            assert!(foreign.active_claim.is_none());

            let report = server.shutdown().await.expect("shutdown worker");
            assert!(report.drained);
            assert_eq!(report.queues, 1);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn duplicate_job_names_are_rejected() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let script = write_script(
                dir.path(),
                r#"
@job("scan")
pub fn scan_a() -> dict { return {} }

@job("scan")
pub fn scan_b() -> dict { return {} }
"#,
            )
            .await;

            let error = run_job_once(&script, "scan", serde_json::json!({}))
                .await
                .expect_err("duplicate job name");
            assert!(error.message().contains("job names must be unique"));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn run_from_files_writes_result_out() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let script = write_script(
                dir.path(),
                r#"
import "std/triggers"

@job("scan")
pub fn scan(harness: Harness, event: TriggerEvent) -> dict {
  return {status: "ok", echo: event.provider_payload.raw}
}
"#,
            )
            .await;
            let request_path = dir.path().join("req.json");
            tokio::fs::write(&request_path, r#"{"k": "v"}"#)
                .await
                .expect("write request");
            let out_path = dir.path().join("out.json");

            let (outcome, rendered) =
                run_job_from_files(&script, "scan", &request_path, Some(&out_path), false)
                    .await
                    .expect("run job from files");

            assert!(outcome.succeeded());
            let written = tokio::fs::read_to_string(&out_path)
                .await
                .expect("read out");
            assert_eq!(written, rendered);
            let parsed: serde_json::Value = serde_json::from_str(&written).expect("parse report");
            assert_eq!(parsed["echo"], serde_json::json!({"k": "v"}));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn missing_job_attribute_is_an_error() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let script = write_script(
                dir.path(),
                r"
pub fn scan(req: dict) -> dict { return req }
",
            )
            .await;
            let error = run_job_once(&script, "scan", serde_json::json!({}))
                .await
                .expect_err("not a job");
            assert!(error.message().contains("no `@job(\"scan\")`"));
        })
        .await;
}
