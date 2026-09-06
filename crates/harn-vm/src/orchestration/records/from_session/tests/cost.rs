//! Provider-call spans, retry attempts, and exact cost aggregation.

use harn_session_store::{AppendEvent, CreateSession, MemorySessionStore};
use serde_json::json;

use super::super::*;
use super::support::*;
#[tokio::test]
async fn an_unpriced_call_is_unknown_not_a_zero_cost_run() {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create session");
    store
        .append(
            &meta.id,
            AppendEvent::new(
                custom("llm_call"),
                transcript_event(
                    "llm_call",
                    json!({
                        "accounting_status": "unknown",
                        "cost_usd": null,
                        "input_tokens": 0,
                        "output_tokens": 0,
                        "provider": "fireworks",
                        "model": "unpriced-model",
                    }),
                ),
            ),
        )
        .await
        .expect("append");

    let run = project_run_record_from_session(&store, &meta.id)
        .await
        .expect("project");
    let usage = run.usage.as_ref().expect("usage");
    assert_eq!(usage.cost_usd, None);
    assert_eq!(usage.known_cost_usd, 0.0);
    assert_eq!(usage.total_cost, 0.0);
    assert_eq!(usage.unpriced_calls, 1);
    assert_eq!(usage.usage_unknown_calls, 1);
}

/// A run report sources its per-call view from `trace_spans`. Leaving them
/// empty made `harn runs report` say a 96-call run made zero LLM calls — worse
/// than a missing field, because zero reads as a measurement.
#[tokio::test]
async fn every_recorded_provider_call_becomes_an_llm_call_span() {
    let (store, id) = capstone_like_store().await;
    let run = project_run_record_from_session(&store, &id)
        .await
        .expect("project");

    let spans: Vec<_> = run
        .evidence
        .trace_spans
        .iter()
        .filter(|span| span.kind == "llm_call")
        .collect();
    assert_eq!(
        spans.len(),
        run.usage.as_ref().expect("usage").call_count as usize,
        "the per-call view and the aggregate must agree on how many calls there were"
    );

    let first = spans[0];
    assert_eq!(first.trace_id, id);
    assert_eq!(first.name, "gpt-5.6-luna");
    assert_eq!(first.cost_usd, Some(0.002872));
    assert_eq!(
        first.metadata.get("input_tokens").and_then(|v| v.as_i64()),
        Some(10951)
    );
    assert_eq!(
        first.metadata.get("model").and_then(|v| v.as_str()),
        Some("gpt-5.6-luna")
    );
    // Span ids must be distinct or the report's `trace:span` call ids collide
    // and the calls dedupe away.
    let ids: std::collections::BTreeSet<_> = spans.iter().map(|span| span.span_id).collect();
    assert_eq!(ids.len(), spans.len());
}

/// A zero duration here is "not recorded", not "instant". Saying so in the span
/// is what keeps a latency view from averaging in 96 fabricated zeroes.
#[tokio::test]
async fn a_projected_span_declares_that_its_duration_is_not_a_measurement() {
    let (store, id) = capstone_like_store().await;
    let run = project_run_record_from_session(&store, &id)
        .await
        .expect("project");

    for span in run
        .evidence
        .trace_spans
        .iter()
        .filter(|s| s.kind == "llm_call")
    {
        assert_eq!(span.duration_ms, 0);
        assert_eq!(
            span.metadata
                .get("duration_available")
                .and_then(|v| v.as_bool()),
            Some(false),
            "a zero duration must be labelled as absent evidence"
        );
        assert_eq!(span.ttft_ms, None);
    }
}

/// Money is a base-10 quantity. Adding the 96 per-call `f64` costs from the
/// real run in #6120 produced `0.6060984600000002`; a run report is read by
/// people reconciling spend against a provider invoice, so the accumulator is
/// exact and the aggregate equals the sum of the per-call spans exactly.
#[tokio::test]
async fn the_cost_aggregate_is_exact_rather_than_float_accumulated() {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create session");
    // Three costs that do not sum cleanly in binary floating point.
    for cost in [0.1, 0.2, 0.3] {
        store
            .append(&meta.id, llm_call(10, 1, cost))
            .await
            .expect("append");
    }

    let run = project_run_record_from_session(&store, &meta.id)
        .await
        .expect("project");
    let usage = run.usage.as_ref().expect("usage");
    assert_eq!(
        usage.total_cost, 0.6,
        "0.1 + 0.2 + 0.3 accumulated as f64 gives 0.6000000000000001"
    );

    // The aggregate and the per-call view must agree, or a reader reconciling
    // one against the other finds a phantom discrepancy.
    let span_total: f64 = run
        .evidence
        .trace_spans
        .iter()
        .filter_map(|span| span.cost_usd)
        .sum();
    assert!((span_total - usage.total_cost).abs() < 1e-9);
}

/// #5847: a run whose provider rejected 47 of 146 requests with a retryable
/// 429 reported 96 clean calls and no contention signal at all. The call count
/// and the request count are different facts and a report has to carry both.
#[tokio::test]
async fn retried_provider_requests_are_visible_alongside_the_call_count() {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create session");
    for event in [
        llm_call_with_attempts(0.01, 3, 2),
        llm_call_with_attempts(0.01, 1, 0),
        llm_call_with_attempts(0.01, 2, 0),
    ] {
        store.append(&meta.id, event).await.expect("append");
    }

    let run = project_run_record_from_session(&store, &meta.id)
        .await
        .expect("project");
    assert_eq!(
        run.usage.as_ref().expect("usage").call_count,
        3,
        "three logical calls"
    );
    let attempts = run
        .metadata
        .get("provider_attempts")
        .expect("a run that retried must say so");
    assert_eq!(attempts.get("total").and_then(|v| v.as_i64()), Some(6));
    assert_eq!(attempts.get("retries").and_then(|v| v.as_i64()), Some(3));
    assert_eq!(
        attempts.get("rate_limited").and_then(|v| v.as_i64()),
        Some(2),
        "rate limiting is the signal that explains a slow or truncated run"
    );
    assert_eq!(attempts.get("other").and_then(|v| v.as_i64()), Some(1));
}

/// A block of zeroes on every clean run would train a reader to skip the one
/// place the contention signal appears.
#[tokio::test]
async fn a_run_that_never_retried_reports_no_attempt_block() {
    let (store, id) = capstone_like_store().await;
    let run = project_run_record_from_session(&store, &id)
        .await
        .expect("project");
    assert!(
        !run.metadata.contains_key("provider_attempts"),
        "no retries means nothing to report"
    );
}

/// Sessions recorded before provider attempts existed carry no entry. Counting
/// them as one request each keeps the total a lower bound instead of reporting
/// fewer requests than there were calls.
#[tokio::test]
async fn calls_recorded_before_attempts_existed_count_as_one_request_each() {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create session");
    // One old-shape call, one new-shape call that retried twice.
    store
        .append(&meta.id, llm_call(100, 10, 0.01))
        .await
        .expect("append");
    store
        .append(&meta.id, llm_call_with_attempts(0.01, 3, 3))
        .await
        .expect("append");

    let run = project_run_record_from_session(&store, &meta.id)
        .await
        .expect("project");
    let attempts = run.metadata.get("provider_attempts").expect("attempts");
    assert_eq!(
        attempts.get("total").and_then(|v| v.as_i64()),
        Some(4),
        "1 (unrecorded, floored to one request) + 3 (recorded)"
    );
    assert_eq!(
        attempts.get("rate_limited").and_then(|v| v.as_i64()),
        Some(3)
    );
}
