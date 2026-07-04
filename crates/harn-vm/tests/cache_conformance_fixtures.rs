//! Committed prompt-cache conformance fixtures (harn#3927).
//!
//! One cache-capable provider (Anthropic: a warm repeat run reads from cache)
//! and one non-cache route (an open-weight OpenRouter model that reports no
//! cache accounting) classify through the shared Harn classifier. No provider
//! keys or account paths — just recorded usage shapes.

use harn_vm::llm::cache_conformance::{
    classify_cache_conformance_fixture, CacheVerdict, PromptCacheSupportStatus,
};

const ANTHROPIC_FIXTURE: &str =
    include_str!("fixtures/cache_conformance/anthropic_cache_capable.json");
const NO_CACHE_FIXTURE: &str = include_str!("fixtures/cache_conformance/local_no_cache.json");

#[test]
fn anthropic_repeat_run_reads_from_cache() {
    let report = classify_cache_conformance_fixture("", "", ANTHROPIC_FIXTURE)
        .expect("anthropic fixture classifies");
    assert_eq!(
        report.support.status,
        PromptCacheSupportStatus::CacheSupported
    );
    assert!(report.support.profile.prompt_caching);
    // The capability path advertises the cache-control requirements downstream
    // consumers need to explain a miss.
    assert!(report.support.profile.min_useful_prefix_tokens.is_some());
    assert!(report.support.profile.ttl_notes.is_some());
    assert_eq!(
        report.support.profile.cache_read_usage_field,
        "usage.cache_read_input_tokens"
    );
    // A repeat run reading from cache is the positive verdict, and it must not
    // fail dogfood.
    assert_eq!(report.verdict, CacheVerdict::CacheEffective);
    assert!(!report.dogfood_failure);
    assert_eq!(report.bucket_counts.cache_effective, 1);
    // Fresh vs cache-read stay separate on the warm run.
    let warm = &report.runs[1];
    assert_eq!(warm.usage.cache_read_tokens, 3800);
    assert_eq!(warm.usage.fresh_input_tokens, 40);
}

#[test]
fn non_cache_route_classifies_unsupported_without_failing_dogfood() {
    let report = classify_cache_conformance_fixture("", "", NO_CACHE_FIXTURE)
        .expect("no-cache fixture classifies");
    assert_eq!(
        report.support.status,
        PromptCacheSupportStatus::CacheUnsupported
    );
    assert!(!report.support.profile.prompt_caching);
    assert_eq!(report.verdict, CacheVerdict::UnsupportedZero);
    assert!(!report.dogfood_failure);
    assert_eq!(report.bucket_counts.unsupported_zero, 2);
    // Absent provider cache fields are recorded as an observation, never proof
    // of no support.
    for run in &report.runs {
        assert!(run
            .usage
            .missing_fields
            .contains(&"cache_read_tokens".to_string()));
    }
}
