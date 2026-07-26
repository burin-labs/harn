use super::*;

pub(super) async fn acquire(
    state_path: PathBuf,
    provider: &str,
    model: &str,
    keys: &[String],
    request: RateLimitRequest,
    consumer_id: Option<&str>,
    session_id: Option<&str>,
    reroute_on_timeout: bool,
) -> Result<(), crate::value::VmError> {
    let buckets = {
        let registry = registry().lock().expect("rate limiter mutex poisoned");
        durable_buckets_for_keys(&registry, keys, request)
    };
    if buckets.is_empty() {
        return Ok(());
    }
    let outcome = if let Some(consumer_id) = consumer_id.filter(|id| !id.trim().is_empty()) {
        let queue_key = buckets
            .first()
            .map(|bucket| bucket.key().to_string())
            .unwrap_or_else(|| provider_key(provider));
        let fair = crate::durable_rate_limit::acquire_fair_durable_rate_limit(
            state_path,
            buckets,
            queue_key,
            consumer_id.to_string(),
            durable_max_wait_ms(),
            FAIR_QUEUE_STARVATION_MS,
            reroute_on_timeout,
            || false,
            |snapshot| {
                emit_fair_queue_fact(
                    provider,
                    model,
                    consumer_id,
                    session_id,
                    "queued",
                    Some(snapshot),
                    &snapshot.counters,
                    0,
                );
            },
        )
        .await?;
        if fair.timed_out && reroute_on_timeout {
            emit_fair_queue_fact(
                provider,
                model,
                consumer_id,
                session_id,
                "rerouted",
                None,
                &fair.counters,
                fair.waited_ms,
            );
            return Err(crate::value::VmError::CategorizedError {
                message: format!(
                    "fair rate-limit queue for '{provider}/{model}' exceeded its \
                     {}ms wait threshold; trying an equivalent route",
                    fair.waited_ms
                ),
                category: crate::value::ErrorCategory::RateLimit,
            });
        }
        emit_fair_queue_fact(
            provider,
            model,
            consumer_id,
            session_id,
            if fair.acquired { "served" } else { "bypassed" },
            None,
            &fair.counters,
            fair.waited_ms,
        );
        crate::durable_rate_limit::DurableRateLimitOutcome {
            acquired: fair.acquired,
            timed_out: fair.timed_out,
            waited_ms: fair.waited_ms,
            retry_after_ms: fair.retry_after_ms,
        }
    } else {
        crate::durable_rate_limit::acquire_durable_rate_limit(
            state_path,
            buckets,
            durable_max_wait_ms(),
            || false,
        )
        .await?
    };
    if outcome.waited_ms > 0 {
        let route = if model.trim().is_empty() {
            provider.to_string()
        } else {
            format!(
                "{provider}/{}",
                crate::llm_config::normalize_model_id(model)
            )
        };
        crate::events::log_debug(
            "llm.rate_limit",
            &format!(
                "Durable rate limit for '{}': waited {}ms",
                route, outcome.waited_ms
            ),
        );
        // The wait was clamped before the quota cleared: proceed to attempt the
        // provider anyway rather than block longer. A genuine over-quota route
        // returns a 429 here, which feeds the Retry-After / retry / escalation
        // path.
        if outcome.timed_out {
            crate::events::log_debug(
                "llm.rate_limit",
                &format!(
                    "Durable rate limit for '{}': wait clamped at {}ms (cap reached); \
                     proceeding to attempt the provider",
                    route, outcome.waited_ms
                ),
            );
        }
    }
    Ok(())
}

fn emit_fair_queue_fact(
    provider: &str,
    model: &str,
    consumer_id: &str,
    session_id: Option<&str>,
    status: &str,
    snapshot: Option<&crate::durable_rate_limit::FairRateLimitSnapshot>,
    counters: &crate::durable_rate_limit::FairRateLimitCounters,
    waited_ms: u64,
) {
    let queue_position = snapshot.map(|value| value.queue_position);
    let expected_wait_ms = snapshot.map(|value| value.expected_wait_ms);
    let fields = serde_json::Map::from_iter([
        (
            "schema".to_string(),
            serde_json::json!("harn.llm.rate_limit_queue.v1"),
        ),
        (
            "receipt_kind".to_string(),
            serde_json::json!("rate_limit_queue"),
        ),
        ("status".to_string(), serde_json::json!(status)),
        ("provider".to_string(), serde_json::json!(provider)),
        ("model".to_string(), serde_json::json!(model)),
        ("consumer_id".to_string(), serde_json::json!(consumer_id)),
        (
            "queue_position".to_string(),
            serde_json::json!(queue_position),
        ),
        (
            "expected_wait_ms".to_string(),
            serde_json::json!(expected_wait_ms),
        ),
        ("waited_ms".to_string(), serde_json::json!(waited_ms)),
        (
            "served_count".to_string(),
            serde_json::json!(counters.served),
        ),
        (
            "queued_count".to_string(),
            serde_json::json!(counters.queued),
        ),
        (
            "rerouted_count".to_string(),
            serde_json::json!(counters.rerouted),
        ),
        (
            "message".to_string(),
            serde_json::json!(match status {
                "queued" => "Waiting fairly for provider capacity",
                "rerouted" => "Provider wait threshold reached; trying an equivalent route",
                "served" => "Provider capacity granted",
                _ => "Queue wait capped; attempting the configured provider",
            }),
        ),
    ]);
    crate::llm::append_observability_sidecar_entry("rate_limit_queue", fields.clone());

    if let Some(session_id) = session_id {
        crate::agent_events::emit_event(&crate::agent_events::AgentEvent::ProgressReported {
            session_id: session_id.to_string(),
            // Keep this on the structured ACP extension path. A non-empty
            // message is reserved for unstructured narration updates.
            message: None,
            entries: serde_json::Value::Object(fields.clone()),
            replace: status == "queued",
            metadata: serde_json::Value::Object(fields),
        });
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::sync::{Arc, Mutex};

    use crate::agent_events::{AgentEvent, AgentEventSink};
    use crate::durable_rate_limit::{FairRateLimitCounters, FairRateLimitSnapshot};
    use crate::value::ErrorCategory;

    struct EnvRestore {
        key: &'static str,
        value: Option<OsString>,
    }

    impl EnvRestore {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self {
                key,
                value: previous,
            }
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            if let Some(value) = self.value.as_ref() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[derive(Default)]
    struct CapturingSink(Mutex<Vec<serde_json::Value>>);

    impl AgentEventSink for CapturingSink {
        fn handle_event(&self, event: &AgentEvent) {
            self.0
                .lock()
                .expect("capture mutex")
                .push(serde_json::to_value(event).expect("serialize event"));
        }
    }

    #[test]
    fn queue_fact_is_a_structured_session_event_with_cumulative_counters() {
        let session_id = "fair-queue-event";
        let sink = Arc::new(CapturingSink::default());
        crate::agent_events::register_sink(session_id, sink.clone());
        let counters = FairRateLimitCounters {
            served: 2,
            queued: 3,
            rerouted: 1,
        };
        let snapshot = FairRateLimitSnapshot {
            queue_position: 4,
            expected_wait_ms: 12_000,
            counters: counters.clone(),
        };

        super::emit_fair_queue_fact(
            "provider",
            "model",
            "tenant-7",
            Some(session_id),
            "queued",
            Some(&snapshot),
            &counters,
            0,
        );
        crate::agent_events::clear_session_sinks(session_id);

        let events = sink.0.lock().expect("capture mutex");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "progress_reported");
        assert!(events[0]["message"].is_null());
        assert_eq!(
            events[0]["metadata"]["schema"],
            "harn.llm.rate_limit_queue.v1"
        );
        assert_eq!(events[0]["metadata"]["queue_position"], 4);
        assert_eq!(events[0]["metadata"]["expected_wait_ms"], 12_000);
        assert_eq!(events[0]["metadata"]["consumer_id"], "tenant-7");
        assert_eq!(events[0]["metadata"]["served_count"], 2);
        assert_eq!(events[0]["metadata"]["queued_count"], 3);
        assert_eq!(events[0]["metadata"]["rerouted_count"], 1);
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "current_thread")]
    async fn bounded_fair_wait_becomes_a_typed_routing_failure() {
        let _env_guard = crate::llm::env_guard();
        let _wait = EnvRestore::set(super::DURABLE_RATE_LIMIT_MAX_WAIT_MS_ENV, "1");
        let _clock = crate::stdlib::clock::MockClockGuard::install(1_000);
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("rate.sqlite");
        super::reset_rate_limit_state();
        super::set_rate_limits(
            "fair-route",
            crate::llm_config::RateLimitsDef {
                rpm: Some(1),
                ..Default::default()
            },
        );
        let keys = super::limiter_keys("fair-route", "");

        super::acquire(
            path.clone(),
            "fair-route",
            "",
            &keys,
            super::RateLimitRequest::default(),
            None,
            None,
            false,
        )
        .await
        .expect("seed shared quota");
        let error = super::acquire(
            path,
            "fair-route",
            "",
            &keys,
            super::RateLimitRequest::default(),
            Some("tenant-b"),
            None,
            true,
        )
        .await
        .expect_err("bounded fair wait should ask routing for an alternate");

        assert_eq!(
            crate::value::error_to_category(&error),
            ErrorCategory::RateLimit
        );
        assert!(error.to_string().contains("trying an equivalent route"));
        super::reset_rate_limit_state();
    }
}
