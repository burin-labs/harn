//! Prompt-cache conformance probe + classifier for Harn providers.
//!
//! The classifier is the stable contract Burin dogfood (#3532) and Harn Cloud
//! receipts (#1106) consume; a live repeat-run HTTP probe is a convenience
//! around it. Given a provider/model and one-or-more repeat runs of a
//! stable-prefix request, this module:
//!
//! - resolves prompt-cache SUPPORT + cache-control requirements from the single
//!   provider capability path ([`crate::llm::capabilities::lookup`]), projecting
//!   a self-describing [`CacheControlProfile`] (breakpoint style, minimum useful
//!   prefix, TTL notes, and the provider usage-field mapping);
//! - normalizes each run's usage keeping fresh-input / cache-read / cache-write /
//!   output / unknown-missing SEPARATE ([`NormalizedCacheUsage`]);
//! - classifies each run into one stable bucket
//!   ([`CacheConformanceClassification`]); and
//! - aggregates a report verdict a repeat run can act on.
//!
//! The taxonomy here is the Harn-owned home for what Burin's
//! `lib/runtime/model-selection.harn` bootstrapped: support classification plus
//! the observation buckets. Product/runtime layers read this one verdict rather
//! than re-deriving provider behavior.
//!
//! A missing provider usage field is recorded as an OBSERVATION
//! ([`NormalizedCacheUsage::missing_fields`]); it never re-classifies a route to
//! "unsupported". Only the capability matrix decides support.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::llm::capabilities::{self, Capabilities, WireDialect};

/// Wire-format version of [`CacheConformanceReport`]. Bump on a breaking shape
/// change so Burin/Cloud consumers can gate on the contract they parse.
pub const CACHE_CONFORMANCE_SCHEMA_VERSION: u32 = 1;

/// Cache-control requirements for a `(provider, model)` route, derived from the
/// single provider capability path. This is the self-describing capability the
/// issue asks Harn to expose: cache-control strategy, minimum useful prefix,
/// TTL notes, and the usage-field mapping — one source, no per-call-site
/// provider branching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheControlProfile {
    /// Whether the route reports prompt-cache accounting at all
    /// ([`Capabilities::prompt_caching`]).
    pub prompt_caching: bool,
    /// Request-side cache breakpoint strategy: `none`, `top_level`, or
    /// `last_block` ([`Capabilities::cache_breakpoint_style`]).
    pub cache_breakpoint_style: String,
    /// Minimum prompt-prefix tokens below which a provider will not create or
    /// serve a cache entry, so a zero cache-read on a short prefix is expected
    /// rather than a miss. `None` when the route reports no cache accounting.
    pub min_useful_prefix_tokens: Option<u32>,
    /// Human-readable cache time-to-live / eviction notes for the route. `None`
    /// when the route reports no cache accounting.
    pub ttl_notes: Option<String>,
    /// Explicit prompt-cache TTL values Harn knows how to request for this
    /// route. Empty means the route may cache, but Harn has no explicit TTL
    /// knob for it.
    pub supported_ttls: Vec<String>,
    /// Provider response usage field that carries cache-read (served-from-cache)
    /// prompt tokens, in dotted path form. Empty when the route reports none.
    pub cache_read_usage_field: String,
    /// Provider response usage field that carries cache-write (cache-creation)
    /// prompt tokens, in dotted path form. Empty when the route neither reports
    /// nor bills a separate cache-write field (OpenAI-style automatic caching).
    pub cache_write_usage_field: String,
}

impl CacheControlProfile {
    /// Derive the cache-control profile from resolved [`Capabilities`]. TTL
    /// notes and the usage-field mapping are wire-dialect facts, so they live
    /// here keyed off the one capability path rather than duplicated per model
    /// row or per call site.
    ///
    /// The minimum cacheable prefix is *not* a dialect fact. On the Anthropic
    /// dialect alone it ranges 512..=4096 tokens and is not monotonic across
    /// generations (Opus 5 caches a 512-token prefix; Opus 4.6 and Haiku 4.5
    /// need 4096). A rule that declares `prompt_cache_min_prefix_tokens`
    /// therefore wins; the per-dialect number below is only the fallback for
    /// routes with no measured floor.
    pub fn from_capabilities(caps: &Capabilities) -> Self {
        if !caps.prompt_caching {
            return Self {
                prompt_caching: false,
                cache_breakpoint_style: caps.cache_breakpoint_style.clone(),
                min_useful_prefix_tokens: None,
                ttl_notes: None,
                supported_ttls: Vec::new(),
                cache_read_usage_field: String::new(),
                cache_write_usage_field: String::new(),
            };
        }
        let (dialect_min_prefix, ttl, read_field, write_field) = match caps.message_wire_format {
            WireDialect::Anthropic => (
                1024,
                "5m default breakpoint TTL; 1h with the extended-cache-ttl beta",
                "usage.cache_read_input_tokens",
                "usage.cache_creation_input_tokens",
            ),
            WireDialect::Gemini => (
                1024,
                "Implicit caching with provider-managed eviction; explicit cachedContent honors a caller TTL",
                "usageMetadata.cachedContentTokenCount",
                "",
            ),
            // OpenAI-compatible routes (including OpenRouter's OpenAI passthrough)
            // cache automatically with no separate cache-write field billed.
            WireDialect::OpenAiCompat => (
                1024,
                "Automatic prefix caching; entries idle-evict after ~5-10 minutes",
                "usage.prompt_tokens_details.cached_tokens",
                "",
            ),
            // Native Ollama reports no cache accounting; a prompt_caching=true
            // rule on this dialect is unexpected, so surface the normalized
            // fields and let the miss classify on capability support.
            WireDialect::Ollama => (0, "No provider-reported cache accounting", "", ""),
        };
        let min_prefix = caps
            .prompt_cache_min_prefix_tokens
            .unwrap_or(dialect_min_prefix);
        Self {
            prompt_caching: true,
            cache_breakpoint_style: caps.cache_breakpoint_style.clone(),
            min_useful_prefix_tokens: if min_prefix > 0 {
                Some(min_prefix)
            } else {
                None
            },
            ttl_notes: if ttl.is_empty() {
                None
            } else {
                Some(ttl.to_string())
            },
            supported_ttls: caps.prompt_cache_ttls.clone(),
            cache_read_usage_field: read_field.to_string(),
            cache_write_usage_field: write_field.to_string(),
        }
    }
}

/// Capability-derived prompt-cache support verdict. `Unknown` is distinct from
/// `Unsupported`: an unresolved provider/model (empty or `auto`) is not proof of
/// no support, matching the missing-field rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptCacheSupportStatus {
    CacheSupported,
    CacheUnsupported,
    CacheSupportUnknown,
}

impl PromptCacheSupportStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CacheSupported => "cache_supported",
            Self::CacheUnsupported => "cache_unsupported",
            Self::CacheSupportUnknown => "cache_support_unknown",
        }
    }
}

/// Prompt-cache support resolved from the provider capability path, plus the
/// cache-control profile consumers need to explain a zero cache-read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptCacheSupport {
    pub status: PromptCacheSupportStatus,
    /// `Some(true)` / `Some(false)` from the capability matrix; `None` when the
    /// provider/model didn't resolve to a concrete route.
    pub supported: Option<bool>,
    /// `provider-prompt-cache` when supported, `none` when explicitly
    /// unsupported, absent when unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_tier: Option<String>,
    pub resolved_provider: String,
    pub resolved_model: String,
    pub source: String,
    pub profile: CacheControlProfile,
}

/// Resolve prompt-cache support for a `(provider, model)` pair from the single
/// provider capability path. An empty or `auto` provider (or empty model)
/// resolves to `Unknown` rather than fabricating an unsupported verdict.
pub fn prompt_cache_support(provider: &str, model: &str) -> PromptCacheSupport {
    let provider_key = provider.trim();
    let model_key = model.trim();
    let unresolved = provider_key.is_empty()
        || provider_key.eq_ignore_ascii_case("auto")
        || model_key.is_empty();
    if unresolved {
        return PromptCacheSupport {
            status: PromptCacheSupportStatus::CacheSupportUnknown,
            supported: None,
            cache_tier: None,
            resolved_provider: provider_key.to_string(),
            resolved_model: model_key.to_string(),
            source: "unresolved".to_string(),
            profile: CacheControlProfile {
                prompt_caching: false,
                cache_breakpoint_style: "none".to_string(),
                min_useful_prefix_tokens: None,
                ttl_notes: None,
                supported_ttls: Vec::new(),
                cache_read_usage_field: String::new(),
                cache_write_usage_field: String::new(),
            },
        };
    }
    let caps = capabilities::lookup(provider_key, model_key);
    let profile = CacheControlProfile::from_capabilities(&caps);
    let (status, cache_tier) = if caps.prompt_caching {
        (
            PromptCacheSupportStatus::CacheSupported,
            Some("provider-prompt-cache".to_string()),
        )
    } else {
        (
            PromptCacheSupportStatus::CacheUnsupported,
            Some("none".to_string()),
        )
    };
    PromptCacheSupport {
        status,
        supported: Some(caps.prompt_caching),
        cache_tier,
        resolved_provider: provider_key.to_string(),
        resolved_model: model_key.to_string(),
        source: "provider-capabilities".to_string(),
        profile,
    }
}

/// Normalized cache usage for one run. Fresh-input, cache-read, cache-write, and
/// output token counts stay SEPARATE; fields the provider omitted are recorded
/// in `missing_fields` as an observation, never folded into a zero that would
/// read as "no support".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedCacheUsage {
    /// Total prompt tokens as the provider reported them (cache-read tokens are
    /// included here on providers that count them toward the prompt total).
    pub input_tokens: i64,
    /// Prompt tokens billed as fresh (non-cached) input: `input - read - write`,
    /// clamped at 0.
    pub fresh_input_tokens: i64,
    /// Prompt tokens served from the provider cache.
    pub cache_read_tokens: i64,
    /// Prompt tokens written to the provider cache on this request.
    pub cache_write_tokens: i64,
    pub output_tokens: i64,
    /// Whether the provider reported any cache accounting field for this run.
    /// `false` means "unknown", not "0% hit".
    pub cache_supported: bool,
    /// Usage fields the provider response did not carry (e.g. `cache_read_tokens`
    /// on a native-Ollama done frame). Diagnostic only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_fields: Vec<String>,
}

fn usage_i64(usage: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<i64> {
    for key in keys {
        if let Some(found) = usage.get(*key).and_then(Value::as_i64) {
            return Some(found);
        }
    }
    None
}

impl NormalizedCacheUsage {
    /// Normalize a usage object that may be Harn's own usage dict shape or a raw
    /// provider usage object. Accepts the provider aliases Harn already reads in
    /// [`crate::llm::jsonl`] and [`crate::llm::api::result`]
    /// (`cache_creation_input_tokens`, `cache_read_input_tokens`,
    /// `prompt_tokens_details.cached_tokens`), so a fixture can be a saved
    /// provider response or a normalized transcript usage entry.
    pub fn from_usage_value(usage: &Value) -> Self {
        let Some(object) = usage.as_object() else {
            return Self {
                input_tokens: 0,
                fresh_input_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                output_tokens: 0,
                cache_supported: false,
                missing_fields: vec!["usage".to_string()],
            };
        };
        let mut missing_fields = Vec::new();

        let input_tokens =
            usage_i64(object, &["input_tokens", "prompt_tokens"]).unwrap_or_else(|| {
                missing_fields.push("input_tokens".to_string());
                0
            });
        let output_tokens = usage_i64(object, &["output_tokens", "completion_tokens"])
            .unwrap_or_else(|| {
                missing_fields.push("output_tokens".to_string());
                0
            });

        // A provider "reports cache accounting" when it carries an explicit
        // read/write field OR an explicit cache_supported flag. Native local
        // runtimes carry neither, so a 0 there is unknown, not a real miss.
        let explicit_supported = object.get("cache_supported").and_then(Value::as_bool);
        let cache_read = usage_i64(
            object,
            &[
                "cache_read_tokens",
                "cache_read_input_tokens",
                "cached_tokens",
            ],
        )
        .or_else(|| nested_cached_tokens(object));
        let cache_write = usage_i64(
            object,
            &["cache_write_tokens", "cache_creation_input_tokens"],
        );
        if cache_read.is_none() {
            missing_fields.push("cache_read_tokens".to_string());
        }
        if cache_write.is_none() {
            missing_fields.push("cache_write_tokens".to_string());
        }
        let cache_read_tokens = cache_read.unwrap_or(0);
        let cache_write_tokens = cache_write.unwrap_or(0);
        let cache_supported = match explicit_supported {
            Some(flag) => flag,
            None => cache_read.is_some() || cache_write.is_some(),
        };
        let fresh_input_tokens = (input_tokens - cache_read_tokens - cache_write_tokens).max(0);
        Self {
            input_tokens,
            fresh_input_tokens,
            cache_read_tokens,
            cache_write_tokens,
            output_tokens,
            cache_supported,
            missing_fields,
        }
    }
}

fn nested_cached_tokens(object: &serde_json::Map<String, Value>) -> Option<i64> {
    object
        .get("prompt_tokens_details")
        .and_then(Value::as_object)
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_i64)
}

/// The stable observation bucket for one repeat run. `ProviderFieldInconsistent`
/// flags a response whose own usage fields contradict each other so a consumer
/// never trusts a cache verdict built on bad numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheConformanceClassification {
    /// Cache-read tokens > 0: the cache served part of the prefix.
    CacheEffective,
    /// Capability says the route caches, but this run read 0 from cache.
    CacheSupportedMiss,
    /// Capability says the route does NOT cache; a 0 read is expected.
    UnsupportedZero,
    /// Capability could not resolve support; a 0 read is inconclusive.
    SupportUnknownZero,
    /// No prompt tokens on the request, so cache behavior is undefined.
    NoPromptTokens,
    /// The run's own usage fields contradict each other (e.g. cache tokens
    /// exceed the prompt total, or a read on a route that flagged no support).
    ProviderFieldInconsistent,
}

impl CacheConformanceClassification {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CacheEffective => "cache_effective",
            Self::CacheSupportedMiss => "cache_supported_miss",
            Self::UnsupportedZero => "unsupported_zero",
            Self::SupportUnknownZero => "support_unknown_zero",
            Self::NoPromptTokens => "no_prompt_tokens",
            Self::ProviderFieldInconsistent => "provider_field_inconsistent",
        }
    }
}

/// Detect a self-contradictory usage report. Returns a human reason when the
/// numbers can't be trusted, else `None`.
fn field_inconsistency(usage: &NormalizedCacheUsage) -> Option<String> {
    if usage.input_tokens < 0
        || usage.output_tokens < 0
        || usage.cache_read_tokens < 0
        || usage.cache_write_tokens < 0
    {
        return Some("negative token count".to_string());
    }
    // A read with no prompt at all can't have come from this prompt's cache.
    if usage.input_tokens <= 0 && (usage.cache_read_tokens > 0 || usage.cache_write_tokens > 0) {
        return Some("cache tokens reported with zero prompt tokens".to_string());
    }
    if usage.input_tokens > 0
        && usage.cache_read_tokens + usage.cache_write_tokens > usage.input_tokens
    {
        return Some("cache-read + cache-write exceed prompt tokens".to_string());
    }
    // Provider both flagged "no cache accounting" AND reported cache tokens.
    if !usage.cache_supported && (usage.cache_read_tokens > 0 || usage.cache_write_tokens > 0) {
        return Some("cache tokens reported while cache_supported=false".to_string());
    }
    None
}

/// Classify one run from its normalized usage and the capability support
/// verdict. Support status — never the presence/absence of a usage field —
/// decides the zero-read bucket, so a missing field can't masquerade as
/// "unsupported".
pub fn classify_cache_run(
    usage: &NormalizedCacheUsage,
    support: &PromptCacheSupport,
) -> CacheConformanceClassification {
    if field_inconsistency(usage).is_some() {
        return CacheConformanceClassification::ProviderFieldInconsistent;
    }
    if usage.input_tokens <= 0 {
        return CacheConformanceClassification::NoPromptTokens;
    }
    if usage.cache_read_tokens > 0 {
        return CacheConformanceClassification::CacheEffective;
    }
    match support.status {
        PromptCacheSupportStatus::CacheSupported => {
            CacheConformanceClassification::CacheSupportedMiss
        }
        PromptCacheSupportStatus::CacheUnsupported => {
            CacheConformanceClassification::UnsupportedZero
        }
        PromptCacheSupportStatus::CacheSupportUnknown => {
            CacheConformanceClassification::SupportUnknownZero
        }
    }
}

/// The stable identity of the request whose prefix must stay fixed across repeat
/// runs for a cache-read to mean anything. Captured (not the raw bytes, which
/// may carry secrets) so a consumer can confirm the runs were actually
/// comparable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheRequestIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_tokens_estimate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_schema_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings_sha256: Option<String>,
}

/// One repeat run: request identity, normalized usage, classification, timing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheConformanceRun {
    pub run_index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<CacheRequestIdentity>,
    pub usage: NormalizedCacheUsage,
    pub classification: CacheConformanceClassification,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inconsistency_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    /// Raw provider usage object as captured, for downstream audit. Preserved
    /// verbatim so a consumer can re-derive without re-running the provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_usage: Option<Value>,
}

/// Report-level cache verdict aggregated across repeat runs — the one signal
/// Burin dogfood and Cloud receipts key on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheVerdict {
    /// A run after the first read from cache: repeat caching works.
    CacheEffective,
    /// Route caches per capability, but no repeat run read from cache.
    CacheSupportedMiss,
    /// Route does not cache per capability; zero reads are expected.
    UnsupportedZero,
    /// Support unknown and no reads observed.
    SupportUnknownZero,
    /// At least one run's usage fields were self-contradictory.
    ProviderFieldInconsistent,
    /// No run carried prompt tokens.
    NoPromptTokens,
    /// Fewer than two runs, so repeat-cache behavior can't be judged.
    InsufficientRuns,
}

impl CacheVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CacheEffective => "cache_effective",
            Self::CacheSupportedMiss => "cache_supported_miss",
            Self::UnsupportedZero => "unsupported_zero",
            Self::SupportUnknownZero => "support_unknown_zero",
            Self::ProviderFieldInconsistent => "provider_field_inconsistent",
            Self::NoPromptTokens => "no_prompt_tokens",
            Self::InsufficientRuns => "insufficient_runs",
        }
    }

    /// Whether this verdict should fail product dogfood. A non-cache provider
    /// classifying as `unsupported_zero` is NOT a failure; only a supported
    /// route that never caches, or a provider reporting contradictory fields,
    /// is a real conformance failure.
    pub fn is_dogfood_failure(self) -> bool {
        matches!(
            self,
            Self::CacheSupportedMiss | Self::ProviderFieldInconsistent
        )
    }
}

/// Per-bucket run counts for report rollups. Mirrors Burin's
/// `prompt_cache_observation_bucket_counts`, now Harn-owned.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheConformanceBucketCounts {
    pub cache_effective: usize,
    pub cache_supported_miss: usize,
    pub unsupported_zero: usize,
    pub support_unknown_zero: usize,
    pub no_prompt_tokens: usize,
    pub provider_field_inconsistent: usize,
}

impl CacheConformanceBucketCounts {
    fn tally(runs: &[CacheConformanceRun]) -> Self {
        let mut counts = Self::default();
        for run in runs {
            match run.classification {
                CacheConformanceClassification::CacheEffective => counts.cache_effective += 1,
                CacheConformanceClassification::CacheSupportedMiss => {
                    counts.cache_supported_miss += 1;
                }
                CacheConformanceClassification::UnsupportedZero => counts.unsupported_zero += 1,
                CacheConformanceClassification::SupportUnknownZero => {
                    counts.support_unknown_zero += 1;
                }
                CacheConformanceClassification::NoPromptTokens => counts.no_prompt_tokens += 1,
                CacheConformanceClassification::ProviderFieldInconsistent => {
                    counts.provider_field_inconsistent += 1;
                }
            }
        }
        counts
    }
}

/// The full conformance report: capability support + per-run observations + one
/// aggregate verdict, consumable by Burin #3532 and Harn Cloud #1106 without
/// reclassifying provider behavior.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheConformanceReport {
    pub schema_version: u32,
    pub provider: String,
    pub model: String,
    pub support: PromptCacheSupport,
    pub runs: Vec<CacheConformanceRun>,
    pub bucket_counts: CacheConformanceBucketCounts,
    pub verdict: CacheVerdict,
    /// Whether `verdict` should fail product dogfood (mirror of
    /// [`CacheVerdict::is_dogfood_failure`], serialized for consumers that read
    /// JSON without the enum semantics).
    pub dogfood_failure: bool,
}

fn aggregate_verdict(runs: &[CacheConformanceRun], support: &PromptCacheSupport) -> CacheVerdict {
    if runs
        .iter()
        .any(|run| run.classification == CacheConformanceClassification::ProviderFieldInconsistent)
    {
        return CacheVerdict::ProviderFieldInconsistent;
    }
    // A repeat run (index > 0) reading from cache is the positive signal; a
    // first-run read alone can't prove repeat caching.
    let repeat_cache_read = runs.iter().any(|run| {
        run.run_index > 0 && run.classification == CacheConformanceClassification::CacheEffective
    });
    if repeat_cache_read {
        return CacheVerdict::CacheEffective;
    }
    // A single run that read from cache (e.g. a warm fixture) still confirms the
    // cache served this prefix.
    let any_cache_read = runs
        .iter()
        .any(|run| run.classification == CacheConformanceClassification::CacheEffective);
    let all_no_prompt = !runs.is_empty()
        && runs
            .iter()
            .all(|run| run.classification == CacheConformanceClassification::NoPromptTokens);
    if all_no_prompt {
        return CacheVerdict::NoPromptTokens;
    }
    match support.status {
        PromptCacheSupportStatus::CacheUnsupported => CacheVerdict::UnsupportedZero,
        PromptCacheSupportStatus::CacheSupportUnknown => CacheVerdict::SupportUnknownZero,
        PromptCacheSupportStatus::CacheSupported => {
            if any_cache_read {
                // Only a first-run read observed; need a repeat to confirm.
                if runs.len() < 2 {
                    CacheVerdict::InsufficientRuns
                } else {
                    CacheVerdict::CacheSupportedMiss
                }
            } else if runs.len() < 2 {
                CacheVerdict::InsufficientRuns
            } else {
                CacheVerdict::CacheSupportedMiss
            }
        }
    }
}

/// Assemble a report from already-classified runs.
pub fn report_from_runs(
    provider: String,
    model: String,
    support: PromptCacheSupport,
    runs: Vec<CacheConformanceRun>,
) -> CacheConformanceReport {
    let bucket_counts = CacheConformanceBucketCounts::tally(&runs);
    let verdict = aggregate_verdict(&runs, &support);
    CacheConformanceReport {
        schema_version: CACHE_CONFORMANCE_SCHEMA_VERSION,
        provider,
        model,
        support,
        runs,
        bucket_counts,
        verdict,
        dogfood_failure: verdict.is_dogfood_failure(),
    }
}

/// Parse one fixture run entry. Accepts either a bare usage object or an entry
/// wrapping `usage` plus optional `request`, `elapsed_ms`, and a `raw_usage`
/// passthrough.
fn run_from_fixture_entry(
    index: usize,
    entry: &Value,
    support: &PromptCacheSupport,
) -> CacheConformanceRun {
    let (usage_value, request, elapsed_ms) = match entry.as_object() {
        Some(object) if object.contains_key("usage") => {
            let usage_value = object.get("usage").cloned().unwrap_or(Value::Null);
            let request = object.get("request").and_then(|value| {
                serde_json::from_value::<CacheRequestIdentity>(value.clone()).ok()
            });
            let elapsed_ms = object.get("elapsed_ms").and_then(Value::as_u64);
            (usage_value, request, elapsed_ms)
        }
        // A bare usage object is the whole entry.
        _ => (entry.clone(), None, None),
    };
    let usage = NormalizedCacheUsage::from_usage_value(&usage_value);
    let classification = classify_cache_run(&usage, support);
    let inconsistency_reason = field_inconsistency(&usage);
    CacheConformanceRun {
        run_index: index,
        request,
        usage,
        classification,
        inconsistency_reason,
        elapsed_ms,
        raw_usage: Some(usage_value),
    }
}

/// Classify a saved repeat-run fixture into a conformance report. `raw` is a
/// JSON document shaped as either a top-level array of run entries or an object
/// with a `runs` array (and optional `provider`/`model` overrides). This is the
/// committed-conformance path: no keys, no live provider, deterministic verdict.
pub fn classify_cache_conformance_fixture(
    provider: impl Into<String>,
    model: impl Into<String>,
    raw: &str,
) -> Result<CacheConformanceReport, String> {
    let document: Value = serde_json::from_str(raw)
        .map_err(|error| format!("failed to parse cache conformance fixture: {error}"))?;
    let mut provider = provider.into();
    let mut model = model.into();
    let runs_value = match &document {
        Value::Array(items) => items.clone(),
        Value::Object(object) => {
            if let Some(fixture_provider) = object.get("provider").and_then(Value::as_str) {
                if provider.trim().is_empty() {
                    provider = fixture_provider.to_string();
                }
            }
            if let Some(fixture_model) = object.get("model").and_then(Value::as_str) {
                if model.trim().is_empty() {
                    model = fixture_model.to_string();
                }
            }
            match object.get("runs") {
                Some(Value::Array(items)) => items.clone(),
                _ => {
                    return Err(
                        "cache conformance fixture object must carry a `runs` array".to_string()
                    )
                }
            }
        }
        _ => {
            return Err(
                "cache conformance fixture must be a runs array or an object with `runs`"
                    .to_string(),
            )
        }
    };
    let support = prompt_cache_support(&provider, &model);
    let runs = runs_value
        .iter()
        .enumerate()
        .map(|(index, entry)| run_from_fixture_entry(index, entry, &support))
        .collect::<Vec<_>>();
    Ok(report_from_runs(provider, model, support, runs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn supported() -> PromptCacheSupport {
        PromptCacheSupport {
            status: PromptCacheSupportStatus::CacheSupported,
            supported: Some(true),
            cache_tier: Some("provider-prompt-cache".to_string()),
            resolved_provider: "anthropic".to_string(),
            resolved_model: "claude-sonnet-4-6".to_string(),
            source: "provider-capabilities".to_string(),
            profile: CacheControlProfile {
                prompt_caching: true,
                cache_breakpoint_style: "last_block".to_string(),
                min_useful_prefix_tokens: Some(1024),
                ttl_notes: Some("5m".to_string()),
                supported_ttls: Vec::new(),
                cache_read_usage_field: "usage.cache_read_input_tokens".to_string(),
                cache_write_usage_field: "usage.cache_creation_input_tokens".to_string(),
            },
        }
    }

    fn unsupported() -> PromptCacheSupport {
        PromptCacheSupport {
            status: PromptCacheSupportStatus::CacheUnsupported,
            supported: Some(false),
            cache_tier: Some("none".to_string()),
            resolved_provider: "ollama".to_string(),
            resolved_model: "qwen3".to_string(),
            source: "provider-capabilities".to_string(),
            profile: CacheControlProfile {
                prompt_caching: false,
                cache_breakpoint_style: "none".to_string(),
                min_useful_prefix_tokens: None,
                ttl_notes: None,
                supported_ttls: Vec::new(),
                cache_read_usage_field: String::new(),
                cache_write_usage_field: String::new(),
            },
        }
    }

    fn unknown() -> PromptCacheSupport {
        prompt_cache_support("auto", "")
    }

    fn usage(input: i64, read: i64, write: i64, output: i64) -> NormalizedCacheUsage {
        NormalizedCacheUsage {
            input_tokens: input,
            fresh_input_tokens: (input - read - write).max(0),
            cache_read_tokens: read,
            cache_write_tokens: write,
            output_tokens: output,
            cache_supported: true,
            missing_fields: Vec::new(),
        }
    }

    #[test]
    fn cache_read_is_effective_regardless_of_support() {
        let run = usage(2000, 1800, 0, 50);
        assert_eq!(
            classify_cache_run(&run, &supported()),
            CacheConformanceClassification::CacheEffective
        );
    }

    #[test]
    fn supported_zero_read_is_a_miss_not_unsupported() {
        let run = usage(2000, 0, 2000, 50);
        assert_eq!(
            classify_cache_run(&run, &supported()),
            CacheConformanceClassification::CacheSupportedMiss
        );
    }

    #[test]
    fn unsupported_zero_read_classifies_unsupported() {
        let run = usage(2000, 0, 0, 50);
        assert_eq!(
            classify_cache_run(&run, &unsupported()),
            CacheConformanceClassification::UnsupportedZero
        );
    }

    #[test]
    fn missing_field_with_unknown_support_stays_unknown_not_unsupported() {
        // Native-local run: no cache fields at all. cache_supported=false is an
        // observation, not proof of no support — the capability path is unknown.
        let raw = json!({ "input_tokens": 2000, "output_tokens": 40 });
        let normalized = NormalizedCacheUsage::from_usage_value(&raw);
        assert!(!normalized.cache_supported);
        assert!(normalized
            .missing_fields
            .contains(&"cache_read_tokens".to_string()));
        assert_eq!(
            classify_cache_run(&normalized, &unknown()),
            CacheConformanceClassification::SupportUnknownZero
        );
    }

    #[test]
    fn no_prompt_tokens_bucket() {
        let run = usage(0, 0, 0, 10);
        assert_eq!(
            classify_cache_run(&run, &supported()),
            CacheConformanceClassification::NoPromptTokens
        );
    }

    #[test]
    fn cache_exceeding_prompt_is_inconsistent() {
        let run = usage(1000, 900, 500, 10);
        assert_eq!(
            classify_cache_run(&run, &supported()),
            CacheConformanceClassification::ProviderFieldInconsistent
        );
    }

    #[test]
    fn read_with_support_false_is_inconsistent() {
        let mut run = usage(2000, 500, 0, 10);
        run.cache_supported = false;
        assert_eq!(
            classify_cache_run(&run, &supported()),
            CacheConformanceClassification::ProviderFieldInconsistent
        );
    }

    #[test]
    fn normalize_reads_anthropic_aliases() {
        let raw = json!({
            "input_tokens": 4000,
            "output_tokens": 120,
            "cache_read_input_tokens": 3500,
            "cache_creation_input_tokens": 500,
        });
        let normalized = NormalizedCacheUsage::from_usage_value(&raw);
        assert_eq!(normalized.cache_read_tokens, 3500);
        assert_eq!(normalized.cache_write_tokens, 500);
        assert_eq!(normalized.fresh_input_tokens, 0);
        assert!(normalized.cache_supported);
        assert!(normalized.missing_fields.is_empty());
    }

    #[test]
    fn normalize_reads_openai_nested_cached_tokens() {
        let raw = json!({
            "prompt_tokens": 3000,
            "completion_tokens": 90,
            "prompt_tokens_details": { "cached_tokens": 2048 },
        });
        let normalized = NormalizedCacheUsage::from_usage_value(&raw);
        assert_eq!(normalized.input_tokens, 3000);
        assert_eq!(normalized.cache_read_tokens, 2048);
        assert_eq!(normalized.fresh_input_tokens, 952);
    }

    #[test]
    fn repeat_run_cache_read_yields_cache_effective_verdict() {
        let raw = json!({
            "provider": "anthropic",
            "model": "claude-sonnet-4-6",
            "runs": [
                { "usage": { "input_tokens": 4000, "output_tokens": 80, "cache_read_tokens": 0, "cache_creation_input_tokens": 3800 } },
                { "usage": { "input_tokens": 4000, "output_tokens": 80, "cache_read_tokens": 3800, "cache_creation_input_tokens": 0 } }
            ]
        });
        let report =
            classify_cache_conformance_fixture("", "", &raw.to_string()).expect("classify");
        assert_eq!(report.verdict, CacheVerdict::CacheEffective);
        assert!(!report.dogfood_failure);
        assert_eq!(report.bucket_counts.cache_effective, 1);
        assert_eq!(report.bucket_counts.cache_supported_miss, 1);
    }

    #[test]
    fn non_cache_provider_does_not_fail_dogfood() {
        let raw = json!({
            "provider": "ollama",
            "model": "qwen3",
            "runs": [
                { "usage": { "input_tokens": 4000, "output_tokens": 80 } },
                { "usage": { "input_tokens": 4000, "output_tokens": 80 } }
            ]
        });
        let report =
            classify_cache_conformance_fixture("", "", &raw.to_string()).expect("classify");
        assert_eq!(report.verdict, CacheVerdict::UnsupportedZero);
        assert!(!report.dogfood_failure);
    }

    #[test]
    fn supported_route_that_never_caches_fails_dogfood() {
        let raw = json!({
            "provider": "anthropic",
            "model": "claude-sonnet-4-6",
            "runs": [
                { "usage": { "input_tokens": 4000, "output_tokens": 80, "cache_creation_input_tokens": 3800 } },
                { "usage": { "input_tokens": 4000, "output_tokens": 80, "cache_creation_input_tokens": 3800 } }
            ]
        });
        let report =
            classify_cache_conformance_fixture("", "", &raw.to_string()).expect("classify");
        assert_eq!(report.verdict, CacheVerdict::CacheSupportedMiss);
        assert!(report.dogfood_failure);
    }
}
