#![allow(dead_code)]

//! In-process [`OrchestratorHarness`] helpers shared by the orchestrator CLI
//! and HTTP integration tests. Event waits use the event-log broadcast
//! channel instead of polling.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use harn_cli::commands::orchestrator::harness::{OrchestratorConfig, OrchestratorHarness};
use harn_cli::env_guard::ScopedEnvVar;
use harn_cli::tests::common::harn_state_lock;
use harn_vm::event_log::{AnyEventLog, EventLog, LogEvent, Topic};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use tempfile::TempDir;

use super::connectors::write_first_party_connector_modules;

/// Hard fail-fast ceiling for event-log waits. The broadcast channel
/// resolves the moment a matching event lands; this just bounds the
/// blast radius if the orchestrator never produces one.
pub const EVENT_FAIL_FAST_TIMEOUT: Duration = Duration::from_secs(30);
pub const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(10);

/// Per-test guard: holds the env-mutation lock and any [`ScopedEnvVar`]
/// guards for the test's lifetime so concurrent tests don't clobber
/// each other's process-wide env vars.
pub struct EnvGuards {
    _lock: harn_state_lock::HarnStateGuard,
    _vars: Vec<ScopedEnvVar>,
}

/// Acquire the env lock and apply the supplied `(name, value)` pairs as
/// scoped env vars. Variables are removed when the returned [`EnvGuards`]
/// drops, after the harness has shut down.
pub async fn lock_env_with(envs: &[(&'static str, &str)]) -> EnvGuards {
    let lock = harn_state_lock::lock_harn_state_async().await;
    let vars = envs
        .iter()
        .map(|(key, value)| ScopedEnvVar::set(key, value))
        .collect();
    EnvGuards {
        _lock: lock,
        _vars: vars,
    }
}

/// Build an [`OrchestratorConfig`] rooted at `temp/harn.toml` with state in
/// `temp/state`. `public_metrics` opts out of the production auth gate on
/// `/metrics` for suites that scrape it directly.
pub fn test_config(temp: &TempDir, public_metrics: bool) -> OrchestratorConfig {
    write_first_party_connector_modules(temp.path());
    let mut config =
        OrchestratorConfig::for_test(temp.path().join("harn.toml"), temp.path().join("state"));
    config.public_metrics = public_metrics;
    config
}

/// Start the harness with [`test_config`] adjusted by `customize`.
pub async fn start_harness_with(
    temp: &TempDir,
    public_metrics: bool,
    customize: impl FnOnce(OrchestratorConfig) -> OrchestratorConfig,
) -> OrchestratorHarness {
    OrchestratorHarness::start(customize(test_config(temp, public_metrics)))
        .await
        .expect("orchestrator harness start")
}

/// Drive a graceful shutdown and assert it completed.
pub async fn shutdown(harness: OrchestratorHarness) {
    harness
        .shutdown(SHUTDOWN_DEADLINE)
        .await
        .expect("orchestrator harness shutdown");
}

/// Subscribe to a topic and wait for the first event matching `predicate`.
/// Hard fail-fast ceiling: [`EVENT_FAIL_FAST_TIMEOUT`].
pub async fn await_topic_event(
    event_log: &Arc<AnyEventLog>,
    topic: &str,
    predicate: impl Fn(&LogEvent) -> bool,
) -> LogEvent {
    await_topic_event_after(event_log, topic, || {}, predicate).await
}

/// Subscribe to a topic, run `trigger`, then wait for the first event matching
/// `predicate`. Use this when `trigger` can emit quickly enough that subscribing
/// afterward would race the broadcast delivery.
pub async fn await_topic_event_after(
    event_log: &Arc<AnyEventLog>,
    topic: &str,
    trigger: impl FnOnce(),
    predicate: impl Fn(&LogEvent) -> bool,
) -> LogEvent {
    let topic_obj = Topic::new(topic).unwrap_or_else(|error| {
        panic!("invalid topic `{topic}`: {error}");
    });
    let mut stream = event_log
        .clone()
        .subscribe(&topic_obj, None)
        .await
        .unwrap_or_else(|error| panic!("subscribe to {topic} failed: {error}"));
    trigger();
    let result = tokio::time::timeout(EVENT_FAIL_FAST_TIMEOUT, async {
        loop {
            let next = stream
                .next()
                .await
                .expect("event stream ended unexpectedly")
                .expect("event stream error");
            if predicate(&next.1) {
                return next.1;
            }
        }
    })
    .await;
    match result {
        Ok(event) => event,
        Err(_) => {
            let recent = event_log
                .read_range(&topic_obj, None, usize::MAX)
                .await
                .unwrap_or_default()
                .into_iter()
                .rev()
                .take(8)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(|(event_id, event)| format!("#{event_id:?} {} {}", event.kind, event.payload))
                .collect::<Vec<_>>()
                .join(" | ");
            panic!("timed out waiting for matching {topic} event; recent events: {recent}");
        }
    }
}

/// Read all events for a topic from the event log.
pub async fn read_topic_events(event_log: &Arc<AnyEventLog>, topic: &str) -> Vec<(u64, LogEvent)> {
    let topic_obj = Topic::new(topic).unwrap_or_else(|error| {
        panic!("invalid topic `{topic}`: {error}");
    });
    event_log
        .read_range(&topic_obj, None, usize::MAX)
        .await
        .unwrap_or_else(|error| panic!("read_range {topic} failed: {error}"))
}

pub fn json_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers
}

pub fn bearer_headers() -> HeaderMap {
    let mut headers = json_headers();
    headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer test-key"));
    headers
}
