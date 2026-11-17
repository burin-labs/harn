//! Cache token counts reach the run record only from calls that reported them.

use harn_session_store::{AppendEvent, CreateSession, MemorySessionStore};
use serde_json::json;

use crate::orchestration::build_run_view;

use super::super::*;
use super::support::*;

fn cached_llm_call(reported: Option<bool>, read: i64, write: i64) -> AppendEvent {
    let mut metadata = json!({
        "cache_read_tokens": read,
        "cache_write_tokens": write,
        "cost_usd": 0.001,
        "input_tokens": 100,
        "model": "gpt-5.6-luna",
        "output_tokens": 10,
        "provider": "openai",
    });
    if let Some(reported) = reported {
        metadata["cache_counts_reported"] = json!(reported);
    }
    if reported == Some(true) {
        let mut totals = crate::llm::usage::ReportedCacheTotals::default();
        totals.record(crate::llm::usage::ReportedCacheUsage {
            read_tokens: Some(read),
            write_tokens: Some(write),
        });
        metadata["reported_cache"] = json!(totals);
    }
    AppendEvent::new(custom("llm_call"), transcript_event("llm_call", metadata))
}

async fn project(calls: Vec<AppendEvent>) -> RunRecord {
    let store = MemorySessionStore::default();
    let meta = store
        .create(CreateSession::default())
        .await
        .expect("create session");
    for call in calls {
        store.append(&meta.id, call).await.expect("append");
    }
    project_run_record_from_session(&store, &meta.id)
        .await
        .expect("project")
}

fn cache_counts(run: &RunRecord) -> (Option<i64>, Option<i64>) {
    let usage = run.usage.as_ref().expect("usage");
    (usage.cache_read_tokens, usage.cache_write_tokens)
}

fn span_cache_keys(run: &RunRecord) -> Vec<(Option<i64>, Option<i64>)> {
    run.evidence
        .trace_spans
        .iter()
        .filter(|span| span.kind == "llm_call")
        .map(|span| {
            let read = |key: &str| span.metadata.get(key).and_then(serde_json::Value::as_i64);
            (read("cache_read_tokens"), read("cache_write_tokens"))
        })
        .collect()
}

#[tokio::test]
async fn reported_cache_counts_sum_into_usage() {
    let run = project(vec![
        cached_llm_call(Some(true), 40, 7),
        cached_llm_call(Some(true), 0, 0),
    ])
    .await;
    assert_eq!(cache_counts(&run), (Some(40), Some(7)));
    assert_eq!(
        span_cache_keys(&run),
        vec![(Some(40), Some(7)), (Some(0), Some(0))],
        "a reported zero is a measurement and stays on the span"
    );
}

#[tokio::test]
async fn unreported_cache_counts_are_unknown_not_zero() {
    // `false` is what the runtime records for an unsupported or undeclared
    // route; the undeclared case may still carry parsed nonzero counts.
    for (read, write) in [(0, 0), (40, 7)] {
        let run = project(vec![cached_llm_call(Some(false), read, write)]).await;
        assert_eq!(cache_counts(&run), (None, None), "counts {read}/{write}");
        assert_eq!(span_cache_keys(&run), vec![(None, None)]);
    }
}

#[tokio::test]
async fn mixed_calls_sum_only_the_reported_ones() {
    let run = project(vec![
        cached_llm_call(Some(false), 99, 5),
        cached_llm_call(Some(true), 40, 7),
        cached_llm_call(None, 11, 3),
    ])
    .await;
    assert_eq!(cache_counts(&run), (Some(40), Some(7)));
}

#[tokio::test]
async fn calls_recorded_before_the_flag_read_as_unreported() {
    let run = project(vec![cached_llm_call(None, 40, 7)]).await;
    assert_eq!(cache_counts(&run), (None, None));
    let encoded = serde_json::to_value(run.usage.as_ref().expect("usage")).expect("encode");
    assert!(
        encoded.get("cache_read_tokens").is_none(),
        "an unknown count is absent from the record, not a zero: {encoded}"
    );
}

#[tokio::test]
async fn partially_reported_categories_stay_absent_through_saved_run_projection() {
    for (read, write, expected) in [
        (None, Some(25), (None, Some(25))),
        (Some(150), None, (Some(150), None)),
        (Some(0), Some(0), (Some(0), Some(0))),
    ] {
        let mut totals = crate::llm::usage::ReportedCacheTotals::default();
        totals.record(crate::llm::usage::ReportedCacheUsage {
            read_tokens: read,
            write_tokens: write,
        });
        let call = AppendEvent::new(
            custom("llm_call"),
            transcript_event(
                "llm_call",
                json!({
                    "cache_read_tokens": read.unwrap_or(0),
                    "cache_write_tokens": write.unwrap_or(0),
                    "cache_counts_reported": true,
                    "reported_cache": totals,
                    "input_tokens": 200,
                    "output_tokens": 1,
                    "provider": "vllm",
                    "model": "cached-model",
                }),
            ),
        );
        let run = project(vec![call]).await;
        assert_eq!(cache_counts(&run), expected);
        assert_eq!(span_cache_keys(&run), vec![expected]);
    }
}

#[tokio::test]
async fn coarse_presence_without_category_measurements_cannot_fabricate_zero() {
    let call = AppendEvent::new(
        custom("llm_call"),
        transcript_event(
            "llm_call",
            json!({
                "cache_read_tokens": 0, "cache_write_tokens": 25,
                "cache_counts_reported": true,
                "input_tokens": 200, "output_tokens": 1,
            }),
        ),
    );
    assert_eq!(cache_counts(&project(vec![call]).await), (None, None));
}

#[test]
fn records_written_before_the_fields_decode_as_unreported() {
    let usage: LlmUsageRecord = serde_json::from_value(json!({
        "input_tokens": 100,
        "output_tokens": 10,
        "call_count": 1,
        "total_cost": 0.001,
    }))
    .expect("decode");
    assert_eq!(usage.cache_read_tokens, None);
    assert_eq!(usage.cache_write_tokens, None);
    assert_eq!(usage.reported_cache, None);
    assert_eq!(usage.provider_call_count, None);
}

fn physical_call(calls: i64, read: Option<i64>) -> AppendEvent {
    let mut cache = crate::llm::usage::ReportedCacheTotals::default();
    cache.record(crate::llm::usage::ReportedCacheUsage {
        read_tokens: read,
        write_tokens: read.map(|_| 0),
    });
    AppendEvent::new(
        custom("llm_call"),
        transcript_event(
            "llm_call",
            json!({
                "input_tokens": 100, "output_tokens": 10, "cost_usd": 0.001,
                "known_cost_usd": 0.001, "unpriced_calls": 0, "usage_unknown_calls": 0,
                "provider_call_count": calls, "reported_cache": cache,
            }),
        ),
    )
}

#[tokio::test]
async fn public_usage_keeps_category_coverage_and_physical_retry_units() {
    for read in [0, 40] {
        let run = project(vec![physical_call(2, Some(read)), physical_call(1, None)]).await;
        let view = build_run_view(&run);
        assert_eq!(view.usage.call_count, 2);
        assert_eq!(view.usage.provider_call_count, Some(3));
        assert_eq!(view.usage.usage_unknown_calls, 0);
        assert_eq!(view.usage.cache_read_tokens, Some(read));
        let cache = view
            .usage
            .reported_cache
            .as_ref()
            .expect("measured coverage");
        assert_eq!(cache.read_reported_calls, 1);
        assert_eq!(cache.write_reported_calls, 1);
        assert_eq!(cache.read_tokens, read);
    }
}

#[tokio::test]
async fn absent_physical_denominator_is_unknown_in_either_order() {
    for calls in [
        vec![physical_call(1, Some(0)), cached_llm_call(Some(true), 0, 0)],
        vec![cached_llm_call(Some(true), 0, 0), physical_call(1, Some(0))],
    ] {
        let view = build_run_view(&project(calls).await);
        assert_eq!(view.usage.call_count, 2);
        assert_eq!(view.usage.provider_call_count, None);
        assert_eq!(view.usage.cache_read_tokens, Some(0));
        assert_eq!(
            view.usage
                .reported_cache
                .as_ref()
                .unwrap()
                .read_reported_calls,
            2
        );
    }
}

#[tokio::test]
async fn measured_no_dispatch_and_unreported_cache_are_distinct_from_legacy() {
    let view = build_run_view(&project(vec![physical_call(0, None)]).await);
    assert_eq!(view.usage.provider_call_count, Some(0));
    assert_eq!(view.usage.reported_cache, Some(Box::default()));
    assert_eq!(view.usage.cache_read_tokens, None);
}
