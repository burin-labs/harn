//! Settlement against the real embedded catalog, at a chosen request instant.
//!
//! These exercise the canonical settlement path — the same functions
//! `LlmUsage::from_result` calls — against catalog rows that are checked in, so
//! a row edit that changes what a call costs shows up here rather than only in
//! a synthetic fixture.

use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use super::api::PromptCacheTtl;
use super::cost::{
    pricing_aware_call_cost, pricing_aware_call_cost_with_cache, pricing_detail_for_tier,
};

fn at(value: &str) -> OffsetDateTime {
    OffsetDateTime::parse(value, &Rfc3339).expect("rfc3339 instant")
}

const DEEPSEEK_PROVIDER: &str = "openrouter";
const DEEPSEEK_MODEL: &str = "deepseek/deepseek-v4-pro-0813";

/// One million input and one million output tokens, so the settled USD figure
/// is the per-MTok rate pair added together and nothing has to be scaled.
fn deepseek_cost(instant: &str) -> f64 {
    pricing_aware_call_cost(
        DEEPSEEK_PROVIDER,
        DEEPSEEK_MODEL,
        1_000_000,
        1_000_000,
        at(instant),
    )
    .expect("deepseek route is priced")
}

fn deepseek_card(instant: &str) -> String {
    pricing_detail_for_tier(
        DEEPSEEK_PROVIDER,
        DEEPSEEK_MODEL,
        false,
        1_000_000,
        at(instant),
    )
    .expect("deepseek route is priced")
    .rate_card
    .label()
}

#[test]
fn a_peak_weekday_instant_settles_on_the_base_card() {
    // 2026-09-21 is a Monday. 02:00Z falls in DeepSeek's 01:00-04:00 peak
    // block, so the base (peak) card applies: 1.32 + 3.96.
    let _guard = super::env_guard();
    assert!((deepseek_cost("2026-09-21T02:00:00Z") - 5.28).abs() < 1e-9);
    assert_eq!(deepseek_card("2026-09-21T02:00:00Z"), "base");
}

#[test]
fn an_off_peak_weekday_instant_settles_at_half_the_base_card() {
    // Same Monday, 05:00Z, inside the 04:00-06:00 off-peak window.
    let _guard = super::env_guard();
    assert!((deepseek_cost("2026-09-21T05:00:00Z") - 2.64).abs() < 1e-9);
    assert_eq!(
        deepseek_card("2026-09-21T05:00:00Z"),
        "schedule:deepseek-offpeak-weekday-0400"
    );
}

#[test]
fn a_weekend_instant_settles_off_peak_at_a_weekday_peak_hour() {
    // 2026-09-26 is a Saturday. 02:00Z is peak on a weekday and off-peak here,
    // which is the fact a day-blind window could not express.
    let _guard = super::env_guard();
    assert!((deepseek_cost("2026-09-26T02:00:00Z") - 2.64).abs() < 1e-9);
    assert_eq!(
        deepseek_card("2026-09-26T02:00:00Z"),
        "schedule:deepseek-offpeak-weekend"
    );
}

#[test]
fn the_three_instants_disagree_because_settlement_reads_the_instant() {
    // The negative control for the three tests above: remove the instant from
    // settlement (pass one fixed instant for all three) and they collapse to
    // the same number. This asserts they do not, so a change that drops the
    // instant fails here rather than reading as a working schedule.
    let _guard = super::env_guard();
    let peak = deepseek_cost("2026-09-21T02:00:00Z");
    let off_peak = deepseek_cost("2026-09-21T05:00:00Z");
    let weekend = deepseek_cost("2026-09-26T02:00:00Z");
    assert!(
        peak > off_peak,
        "peak {peak} must exceed off-peak {off_peak}"
    );
    assert!((off_peak - weekend).abs() < 1e-9);
    assert!((peak - off_peak * 2.0).abs() < 1e-9);
}

#[test]
fn a_promotion_expires_at_its_instant_not_at_the_end_of_its_day() {
    // The GLM 5.3 Flash launch promotion ends at 2026-09-09T16:00:00Z. A
    // session that runs across that instant must price the two calls
    // differently; resolving the card once at catalog load gave both the same.
    let _guard = super::env_guard();
    let before = pricing_aware_call_cost(
        "zai",
        "glm-5.3-flash",
        1_000_000,
        1_000_000,
        at("2026-09-09T15:59:59Z"),
    )
    .expect("priced");
    let after = pricing_aware_call_cost(
        "zai",
        "glm-5.3-flash",
        1_000_000,
        1_000_000,
        at("2026-09-09T16:00:00Z"),
    )
    .expect("priced");
    // Promotion: 0.075 + 0.25. Base: 0.15 + 0.50.
    assert!((before - 0.325).abs() < 1e-9, "promotional settle {before}");
    assert!((after - 0.65).abs() < 1e-9, "base settle {after}");
    assert_eq!(
        pricing_detail_for_tier("zai", "glm-5.3-flash", false, 0, at("2026-09-09T15:59:59Z"))
            .expect("priced")
            .rate_card
            .label(),
        "promotion:zai-glm-5.3-flash-launch"
    );
    assert_eq!(
        pricing_detail_for_tier("zai", "glm-5.3-flash", false, 0, at("2026-09-09T16:00:00Z"))
            .expect("priced")
            .rate_card
            .label(),
        "base"
    );
}

const CLAUDE_PROVIDER: &str = "anthropic";
const CLAUDE_MODEL: &str = "claude-sonnet-4-5-20250929";

/// One million cache-write tokens and nothing else, so the settled figure is
/// the cache-write rate.
fn claude_cache_write_cost(ttl: Option<PromptCacheTtl>) -> f64 {
    pricing_aware_call_cost_with_cache(
        CLAUDE_PROVIDER,
        CLAUDE_MODEL,
        1_000_000,
        0,
        0,
        1_000_000,
        at("2026-09-21T12:00:00Z"),
        ttl,
    )
    .expect("claude route is priced")
}

#[test]
fn a_one_hour_cache_write_settles_above_the_five_minute_write() {
    // Claude Sonnet 4.5: 3.00 input, 3.75 five-minute write (1.25x), 6.00
    // one-hour write (2x). Harn already sends `cache_control.ttl = "1h"`; this
    // is the settlement side of that request.
    let _guard = super::env_guard();
    let five_minutes = claude_cache_write_cost(Some(PromptCacheTtl::FiveMinutes));
    let one_hour = claude_cache_write_cost(Some(PromptCacheTtl::OneHour));
    assert!(
        (five_minutes - 3.75).abs() < 1e-9,
        "5m settle {five_minutes}"
    );
    assert!((one_hour - 6.00).abs() < 1e-9, "1h settle {one_hour}");
    // The negative control: drop the TTL selection and both settle at 1.25x.
    assert!((claude_cache_write_cost(None) - 3.75).abs() < 1e-9);
}

#[test]
fn a_route_with_no_one_hour_tier_settles_short_and_says_so() {
    // A route the catalog prices with one cache-write rate cannot price a
    // one-hour write. It settles at the short rate — the provider may well
    // charge more — and the receipt carries `cache_ttl_unpriced` so the
    // under-count is visible instead of silent.
    let _guard = super::env_guard();
    let detail = pricing_detail_for_tier(
        "openai",
        "gpt-5.6-sol",
        false,
        1_000_000,
        at("2026-09-21T12:00:00Z"),
    )
    .expect("priced");
    assert!(
        detail.cache_write_per_1k.is_some(),
        "fixture route must price a short cache write"
    );
    assert!(detail.cache_write_priced(Some(PromptCacheTtl::FiveMinutes)));
    assert!(!detail.cache_write_priced(Some(PromptCacheTtl::OneHour)));
    let five_minutes = pricing_aware_call_cost_with_cache(
        "openai",
        "gpt-5.6-sol",
        1_000_000,
        0,
        0,
        1_000_000,
        at("2026-09-21T12:00:00Z"),
        Some(PromptCacheTtl::FiveMinutes),
    );
    let one_hour = pricing_aware_call_cost_with_cache(
        "openai",
        "gpt-5.6-sol",
        1_000_000,
        0,
        0,
        1_000_000,
        at("2026-09-21T12:00:00Z"),
        Some(PromptCacheTtl::OneHour),
    );
    assert_eq!(five_minutes, one_hour);
}
