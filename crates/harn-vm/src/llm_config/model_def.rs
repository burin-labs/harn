//! Model catalog DTOs: per-route serving definitions and the sub-records
//! (pricing, rate limits, serving performance, architecture, serving tiers,
//! local runtime/memory, and aliases) that make up a `ModelDef`.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct HealthcheckDef {
    pub method: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct LocalRuntimeDef {
    /// Lifecycle style: `daemon_api` for runtimes with their own resident
    /// daemon (Ollama), `managed_process` for Harn-spawned servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Command Harn should execute for managed-process runtimes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Arguments that must appear immediately after the command, before model
    /// and server flags. Used by CLIs such as `vllm serve ...`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prefix_args: Vec<String>,
    /// Default model source/path/repo. User overlays may set this; embedded
    /// catalog rows avoid machine-specific absolute paths except examples.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_source: Option<String>,
    /// Environment variable that can provide a model source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_source_env: Option<String>,
    /// Default port when the provider base URL has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_port: Option<u16>,
    /// Argument names used by the runtime CLI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_arg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub served_model_arg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_arg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port_arg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx_arg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_arg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_layers_arg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_type_k_arg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_type_v_arg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_ram_arg: Option<String>,
    /// Flag that enables adapter-aware serving for LoRA-capable runtimes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_lora_arg: Option<String>,
    /// Flag that accepts one or more LoRA module specs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lora_modules_arg: Option<String>,
    /// Runtime value shape for LoRA module specs. Defaults to `name_path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lora_modules_value_format: Option<String>,
    /// Optional rank-limit flag for runtimes that need an explicit ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_lora_rank_arg: Option<String>,
    /// Extra arguments Harn applies by default when launching this runtime.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub default_args: Vec<String>,
    /// Stop strategy: `keep_alive_zero`, `pid`, or `external`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<String>,
    /// Official docs/source URL for the lifecycle contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// YYYY-MM-DD date when the local runtime row was last verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verified: Option<String>,
    /// Short operational note surfaced by CLI docs/help.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct LocalMemoryDef {
    /// Empirical resident memory observed for this route/runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_resident_gib: Option<f64>,
    /// Context size used for the empirical measurement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_context_window: Option<u64>,
    /// KV-cache type used for the empirical measurement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_cache_type: Option<String>,
    /// Approximate non-context resident footprint for this model/runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_resident_gib: Option<f64>,
    /// Approximate GiB consumed by KV cache per 1,000 context tokens at the
    /// default cache type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kv_cache_gib_per_1k_ctx: Option<f64>,
    /// Cache-type multiplier relative to `kv_cache_gib_per_1k_ctx`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cache_type_multipliers: BTreeMap<String, f64>,
    /// Cache type assumed when the launch command does not set K/V cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_cache_type: Option<String>,
    /// Minimum headroom Harn should leave for the OS and other apps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety_margin_gib: Option<f64>,
    /// Highest context Harn should recommend automatically from this row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_recommended_context: Option<u64>,
    /// Official or empirical source for the sizing row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// YYYY-MM-DD date when the sizing row was last verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verified: Option<String>,
    /// Short operational note surfaced by CLI diagnostics/docs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl LocalMemoryDef {
    pub fn is_empty(&self) -> bool {
        self.measured_resident_gib.is_none()
            && self.measured_context_window.is_none()
            && self.measured_cache_type.is_none()
            && self.base_resident_gib.is_none()
            && self.kv_cache_gib_per_1k_ctx.is_none()
            && self.cache_type_multipliers.is_empty()
            && self.default_cache_type.is_none()
            && self.safety_margin_gib.is_none()
            && self.max_recommended_context.is_none()
            && self.source_url.is_none()
            && self.last_verified.is_none()
            && self.notes.is_none()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct AliasDef {
    pub id: String,
    pub provider: String,
    /// Per-model tool format override: "native" or "text". When set, this
    /// takes precedence over the provider-level default. Models with strong
    /// tool-calling fine-tuning (Kimi-K2.5, GPT-4o) should use "native";
    /// models better served by text-based tool calling use "text".
    #[serde(default)]
    pub tool_format: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct AliasToolCallingDef {
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native: Option<String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub streaming_native: Option<String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_mode: Option<String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_probe_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ModelPricing {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    #[serde(default)]
    pub cache_read_per_mtok: Option<f64>,
    #[serde(default)]
    pub cache_write_per_mtok: Option<f64>,
    /// Whole-request pricing that activates once provider-reported input usage
    /// reaches a threshold. Providers such as OpenAI and Gemini charge every
    /// token in a long-context request at the selected band's rates rather
    /// than applying marginal pricing only above the boundary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_token_bands: Vec<InputTokenPricingBand>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct InputTokenPricingBand {
    /// Inclusive lower bound for this whole-request rate.
    pub minimum_input_tokens: u64,
    pub input_multiplier: f64,
    pub output_multiplier: f64,
}

impl ModelPricing {
    /// Resolve the whole-request rates for provider-reported input usage.
    /// `max_by_key` keeps runtime selection correct even before catalog
    /// validation reports an authoring-order mistake.
    pub fn for_input_tokens(&self, input_tokens: i64) -> Self {
        let input_tokens = u64::try_from(input_tokens).unwrap_or(0);
        let Some(band) = self
            .input_token_bands
            .iter()
            .filter(|band| band.minimum_input_tokens <= input_tokens)
            .max_by_key(|band| band.minimum_input_tokens)
        else {
            return self.clone();
        };
        Self {
            input_per_mtok: self.input_per_mtok * band.input_multiplier,
            output_per_mtok: self.output_per_mtok * band.output_multiplier,
            cache_read_per_mtok: self
                .cache_read_per_mtok
                .map(|rate| rate * band.input_multiplier),
            cache_write_per_mtok: self
                .cache_write_per_mtok
                .map(|rate| rate * band.input_multiplier),
            input_token_bands: self.input_token_bands.clone(),
        }
    }
}

/// Provider or model quota metadata. Providers publish these along several
/// axes, and any one exhausted bucket can trigger throttling.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct RateLimitsDef {
    /// Requests per minute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpm: Option<u32>,
    /// Requests per hour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rph: Option<u32>,
    /// Requests per day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpd: Option<u32>,
    /// Total tokens per minute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tpm: Option<u64>,
    /// Total tokens per hour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tph: Option<u64>,
    /// Total tokens per day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tpd: Option<u64>,
    /// Input tokens per minute, when the provider splits input/output quotas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tpm: Option<u64>,
    /// Output tokens per minute, when the provider splits input/output quotas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tpm: Option<u64>,
    /// Concurrent in-flight requests, if published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<u32>,
    /// Account tier or route class these limits describe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// Official source URL for the row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// YYYY-MM-DD date when the row was last verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verified: Option<String>,
    /// Free-text caveat for account-dependent or burst limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl RateLimitsDef {
    pub fn is_empty(&self) -> bool {
        self.rpm.is_none()
            && self.rph.is_none()
            && self.rpd.is_none()
            && self.tpm.is_none()
            && self.tph.is_none()
            && self.tpd.is_none()
            && self.input_tpm.is_none()
            && self.output_tpm.is_none()
            && self.concurrency.is_none()
            && self.tier.is_none()
            && self.source_url.is_none()
            && self.last_verified.is_none()
            && self.notes.is_none()
    }

    pub fn with_rpm_fallback(mut self, rpm: Option<u32>) -> Option<Self> {
        if self.rpm.is_none() {
            self.rpm = rpm;
        }
        (!self.is_empty()).then_some(self)
    }
}

/// Optional provider/model serving-performance observation. This records
/// benchmark or live-probe facts, not a hard runtime contract; callers should
/// treat missing fields as unknown and stale dates as advisory.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct ServingPerformanceDef {
    /// Observed time-to-first-token in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_ttft_ms: Option<u64>,
    /// Observed output generation rate in tokens per second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens_per_sec: Option<f64>,
    /// End-to-end time-to-answer in seconds for the cited benchmark, when
    /// reported separately from TTFT/generation rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_to_answer_s: Option<f64>,
    /// Source label, e.g. `artificial_analysis`, `harn_probe`, or
    /// `provider_blog`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Source URL for the observation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// YYYY-MM-DD date when the observation was last verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verified: Option<String>,
    /// Number of requests or benchmark samples behind this row, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_size: Option<u32>,
    /// Short caveat such as streaming mode, warm/cold route, or prompt shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl ServingPerformanceDef {
    pub fn is_empty(&self) -> bool {
        self.observed_ttft_ms.is_none()
            && self.output_tokens_per_sec.is_none()
            && self.time_to_answer_s.is_none()
            && self.source.is_none()
            && self.source_url.is_none()
            && self.last_verified.is_none()
            && self.sample_size.is_none()
            && self.notes.is_none()
    }
}

/// Logical-model facts separated from provider serving routes. These fields
/// describe the underlying weights or public model family, not Harn's alias or
/// provider/model selector.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct ModelArchitectureDef {
    /// Total parameter count in billions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameter_count_b: Option<f64>,
    /// Active parameter count in billions for MoE models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_parameter_count_b: Option<f64>,
    /// True for mixture-of-experts models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moe: Option<bool>,
    /// Quantization advertised by this route, if route-specific.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantization: Option<String>,
    /// Numeric precision advertised by this route, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precision: Option<String>,
    /// License identifier or short label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Tokenizer family or implementation hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokenizer: Option<String>,
    /// Public knowledge cutoff claim, when published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_cutoff: Option<String>,
    /// Official source URL for these facts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// YYYY-MM-DD date when these facts were last verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verified: Option<String>,
}

impl ModelArchitectureDef {
    pub fn is_empty(&self) -> bool {
        self.parameter_count_b.is_none()
            && self.active_parameter_count_b.is_none()
            && self.moe.is_none()
            && self.quantization.is_none()
            && self.precision.is_none()
            && self.license.is_none()
            && self.tokenizer.is_none()
            && self.knowledge_cutoff.is_none()
            && self.source_url.is_none()
            && self.last_verified.is_none()
    }
}

/// Provider request knob that selects a non-default serving tier.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ServingTierRequestDef {
    /// Request field that opts into the tier (for example `speed` for
    /// Anthropic or `service_tier` for OpenAI/Gemini).
    pub param: String,
    /// Value to send on `param` (for example `fast`, `flex`, or `priority`).
    pub value: String,
    /// Provider beta/feature header required to use the tier, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beta_header: Option<String>,
}

/// Whether a serving tier is synchronous request handling or some other
/// provider execution lane. Batch APIs remain represented by the separate
/// async `batch` capability; discounted synchronous lanes such as Gemini Flex
/// belong here instead of overloading `batch_api`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServingTierMode {
    Synchronous,
}

/// Economic shape of a serving tier relative to the default synchronous API.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServingTierEconomics {
    Discounted,
    Standard,
    Premium,
}

/// Optional non-default synchronous serving tier for a model. Off by default:
/// its presence only describes provider capability. Callers must explicitly
/// opt in via the declared request knob, so nothing here changes default
/// behavior. Batch APIs are intentionally not modeled here; they remain the
/// separate async `batch` capability used by `harn models batch`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ServingTierDef {
    /// Stable tier id, e.g. `fast`, `flex`, or `priority`.
    pub id: String,
    /// Human-readable display label for CLI/catalog renderers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub mode: ServingTierMode,
    pub economics: ServingTierEconomics,
    /// Request knob for tiers selected per request. Some tiers may be
    /// informational/account-level only and omit a knob.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<ServingTierRequestDef>,
    /// Output-tokens-per-second speedup vs standard serving (e.g. 2.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otps_speedup: Option<f64>,
    /// Price multiplier relative to default synchronous rates, when public.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_multiplier: Option<f64>,
    /// Discount percentage relative to default synchronous rates, when public.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discount_percent: Option<u32>,
    /// Lifecycle of the tier: "ga" | "research_preview" | "deprecated".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Absolute per-MTok rates charged while the tier is active. Prefer this
    /// over a multiplier when the provider prices the tier asymmetrically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<ModelPricing>,
    /// Latency expectation for humans and planners.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency: Option<String>,
    /// Reliability/availability expectation for humans and planners.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reliability: Option<String>,
    /// Quota-pool or eligibility notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<String>,
    /// Workloads this tier is suitable for (e.g. `offline_eval`, `corpus`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suitable_workloads: Vec<String>,
    /// Workloads this tier should generally avoid (e.g. `interactive_chat`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unsuitable_workloads: Vec<String>,
    /// Free-text note: constraints, deprecation timeline, cache behavior, etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A named model-fallback ladder declared in the catalog under
/// `[model_ladders.<name>]`. A `models`/`ladder` option on `llm_call`
/// lowers a ladder onto the first-class `routing_policy` chain: each step
/// is one transport attempt, and the loop advances to the next step ONLY
/// on transport-class failures (connection/timeout/429/5xx/throttled).
///
/// This data-driven declaration follows the same spirit as `serving_tiers`
/// (#4017): a capability/behavior encoded as catalog data rather than
/// hand-rolled at each downstream call site (harn-bump-fleet,
/// harn-cloud free_tier_pool, burin-code all shipped their own copy).
// NB: `PartialEq` only (no `Eq`): `ModelLadderStepDef::options` holds
// `toml::Value`, which carries floats and therefore is not `Eq`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct ModelLadderDef {
    /// Ordered ladder steps, cheapest/first to most-capable/last.
    #[serde(default)]
    pub steps: Vec<ModelLadderStepDef>,
    /// Optional human-readable label surfaced on the routing envelope.
    #[serde(default)]
    pub label: Option<String>,
}

/// One rung of a [`ModelLadderDef`]. Full parity with the `.harn`
/// `ModelLadderStep` alias — `{model, provider?, label?, when?, options?,
/// family?, capabilities?}` — which is also the shape accepted by the
/// `models:` option and the `model_ladder(...)` std constructor. Provider is
/// optional: when omitted it is inferred from the model id (or the call's base
/// provider) at lowering time.
///
/// `options` carries per-step sampling/timeout overrides (same allowlist as
/// inline `models:` steps); catalog ladders honor them identically instead of
/// silently dropping them. `when`, `family`, and `capabilities` are
/// informational to Harn's own ladder lowering (they do not affect transport
/// failover) but are carried through so catalog and inline ladders declare the
/// same shape and downstream selectors (e.g. harn-cloud free-tier routing) can
/// read them. All added fields are optional and serde-absent when unset.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct ModelLadderStepDef {
    pub model: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    /// Conditional-routing predicate hint (e.g. `"transport_failure"`). Mirror
    /// of the `.harn` alias `when?` field. Informational to lowering today.
    /// Absent from serialized output when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// Per-step sampling/timeout overrides (temperature, max_tokens, top_p,
    /// seed, timeout_ms, fast, ...), same allowlist as inline `models:` steps.
    /// Absent from serialized output when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<BTreeMap<String, toml::Value>>,
    /// Normalized model-family token (e.g. `"haiku"`, `"sonnet"`) carried for
    /// downstream selectors such as harn-cloud's free-tier routing. Purely
    /// informational to Harn's own ladder lowering — it does not affect
    /// transport failover. Absent from serialized output when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// Capability tags this rung claims (e.g. `["vision", "tools"]`). Carried
    /// for downstream capability-aware routing; informational to Harn's own
    /// ladder lowering. Absent from serialized output when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ModelDef {
    pub name: String,
    /// One-sentence, plain-language trade-off description for model pickers.
    #[serde(default)]
    pub blurb: Option<String>,
    pub provider: String,
    pub context_window: u64,
    /// Provider-independent logical model id, when multiple serving routes map
    /// to the same weights or model family.
    #[serde(default)]
    pub logical_model: Option<String>,
    /// Equivalence class for failover/escalation candidates. Entries in the
    /// same group are capability-compatible alternatives, not byte-identical
    /// APIs; callers must still re-render transcripts for the target provider.
    #[serde(default)]
    pub equivalence_group: Option<String>,
    /// Serving-route detail such as "serverless", "priority", "fp8", or a
    /// provider route slug. This is intentionally separate from `name`.
    #[serde(default)]
    pub served_variant: Option<String>,
    /// Provider-native model id to send on the wire. Defaults to the catalog
    /// key. Required when two providers expose the same native id and Harn
    /// needs a unique catalog key for each route.
    #[serde(default)]
    pub wire_model: Option<String>,
    /// Preferred API dialect for the route, e.g. `openai_chat`,
    /// `openai_responses`, `anthropic_messages`, `gemini_generate_content`.
    #[serde(default)]
    pub api_dialect: Option<String>,
    /// Route-specific token/request quota metadata.
    #[serde(default)]
    pub rate_limits: Option<RateLimitsDef>,
    /// Optional route-level serving performance observations.
    #[serde(default)]
    pub performance: Option<ServingPerformanceDef>,
    /// Underlying model architecture facts separated from the provider id.
    #[serde(default)]
    pub architecture: Option<ModelArchitectureDef>,
    /// Local launch memory-sizing hints used by `harn local launch`.
    #[serde(default)]
    pub local_memory: Option<LocalMemoryDef>,
    #[serde(default)]
    pub runtime_context_window: Option<u64>,
    #[serde(default)]
    pub stream_timeout: Option<f64>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub pricing: Option<ModelPricing>,
    #[serde(default)]
    pub deprecated: bool,
    #[serde(default)]
    pub deprecation_note: Option<String>,
    /// Structured replacement pointer: the catalog id of the model that
    /// supersedes this one (e.g. an older Opus row points at the newest
    /// Opus). Lets release tooling express "migrate to X" in a
    /// machine-readable way instead of burying it in `deprecation_note`
    /// free text. A model may be superseded without being `deprecated`
    /// (a newer option exists but this one is still fully supported);
    /// pair it with `deprecated = true` once a sunset is announced.
    #[serde(default)]
    pub superseded_by: Option<String>,
    /// Non-default synchronous serving tiers exposed by the provider, such as
    /// premium fast/priority queues or discounted best-effort Flex lanes. Off
    /// by default — see [`ServingTierDef`]. Empty for models with no alternate
    /// synchronous serving path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub serving_tiers: Vec<ServingTierDef>,
    /// Loose catalog annotations for selectors and UI. Conventional tags
    /// include `avoid_reviewer` for routes that should not be auto-selected as
    /// independent reviewers even when they are routable and cheap.
    #[serde(default)]
    pub quality_tags: Vec<String>,
    /// Whether the model can be reached over a normal API-key serverless call,
    /// or only via a dedicated/provisioned endpoint that the caller must spin
    /// up out-of-band. Providers like Together list dedicated-only routes
    /// alongside serverless ones in `/v1/models`, so this metadata lets clients
    /// avoid presenting them as one-click options.
    #[serde(default)]
    pub availability: ModelAvailability,
    /// Popular-consensus tier label. Enum-typed string: "small" | "mid" |
    /// "frontier" | "reasoning". Self-declared per model (no pattern-matched
    /// rule table) so the catalog is the single source of truth. When None
    /// the resolver returns the catalog default ("mid"). Use the richer
    /// `strengths` + `benchmarks` fields to pick models for specific
    /// workloads — `tier` exists only as a coarse popular-consensus shortcut.
    #[serde(default)]
    pub tier: Option<String>,
    /// True when the model weights are downloadable / self-hostable
    /// (open-weight / open-source license, regardless of commercial-use
    /// restrictions). False when weights are closed (Anthropic, OpenAI,
    /// Google, etc.). None when the catalog row predates the migration.
    #[serde(default)]
    pub open_weight: Option<bool>,
    /// Workload-shaped strength tags. Conventional values include
    /// `coding`, `summarization`, `long_context`, `tool_use`, `reasoning`,
    /// `vision`, `speed`, `cheap`, `agentic`. Selectors should treat
    /// missing entries as "no claim" rather than "no strength."
    #[serde(default)]
    pub strengths: Vec<String>,
    /// Public benchmark numbers, keyed by a snake_case identifier
    /// (`swe_bench_verified`, `humaneval`, `aa_intelligence_index`, etc.).
    /// Values are the raw published scores. The selector layer is free
    /// to normalize per benchmark; the catalog records the canonical
    /// score so future readers can audit the source.
    #[serde(default)]
    pub benchmarks: BTreeMap<String, f64>,
    /// Normalized model-family token used as a diversity signal for
    /// reviewer selection. Distinct from provider: hosted wrappers should
    /// keep the underlying family (for example OpenRouter-hosted Claude
    /// still uses `anthropic-claude`).
    #[serde(default)]
    pub family: Option<String>,
    /// Narrower family lineage used by option-pack calibration.
    #[serde(default)]
    pub lineage: Option<String>,
    /// Preferred reviewer families for critique/review workloads.
    #[serde(default)]
    pub complementary_with: Vec<String>,
    /// Author families, lineages, model ids, or provider/model selectors
    /// this row should not review.
    #[serde(default)]
    pub avoid_as_reviewer_for: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelAvailability {
    /// Reachable through the provider's normal API-key path with no extra
    /// setup. The default for cataloged hosted/local models: by cataloging a
    /// row we are claiming the route works out of the box.
    #[default]
    Serverless,
    /// Requires the caller to provision a dedicated endpoint before requests
    /// will succeed. The catalog row exists for selection/pricing UI, but
    /// hosts must not auto-route to it.
    Dedicated,
    /// Availability is not known ahead of time. Used for routes that were
    /// surfaced dynamically (e.g. through `/v1/models`) without a static
    /// claim from Harn or the user.
    Unknown,
}

impl ModelAvailability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Serverless => "serverless",
            Self::Dedicated => "dedicated",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "serverless" => Some(Self::Serverless),
            "dedicated" => Some(Self::Dedicated),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

#[cfg(test)]
mod ladder_step_tests {
    use super::{ModelLadderDef, ModelLadderStepDef};

    #[test]
    fn all_added_fields_round_trip() {
        let mut options = std::collections::BTreeMap::new();
        options.insert("temperature".to_string(), toml::Value::Float(0.2));
        options.insert("max_tokens".to_string(), toml::Value::Integer(512));
        let step = ModelLadderStepDef {
            model: "claude-haiku-4-5".to_string(),
            provider: Some("anthropic".to_string()),
            label: Some("cheap".to_string()),
            when: Some("transport_failure".to_string()),
            options: Some(options),
            family: Some("haiku".to_string()),
            capabilities: vec!["vision".to_string(), "tools".to_string()],
        };
        let json = serde_json::to_string(&step).expect("serialize");
        let back: ModelLadderStepDef = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(step, back);
        assert!(json.contains("\"family\":\"haiku\""));
        assert!(json.contains("\"capabilities\":[\"vision\",\"tools\"]"));
        assert!(json.contains("\"when\":\"transport_failure\""));
        assert!(json.contains("\"temperature\":0.2"));
    }

    #[test]
    fn unset_added_fields_are_absent_from_serialized_output() {
        // A step that sets none of the added fields must serialize
        // byte-identically to the pre-existing {model, provider?, label?}
        // shape: the new keys are entirely absent (not `null`, not `[]`), so
        // already-serialized catalog bundles/records stay unchanged.
        let step = ModelLadderStepDef {
            model: "mock-cheap".to_string(),
            provider: Some("mock".to_string()),
            label: None,
            when: None,
            options: None,
            family: None,
            capabilities: Vec::new(),
        };
        let json = serde_json::to_string(&step).expect("serialize");
        assert_eq!(
            json,
            r#"{"model":"mock-cheap","provider":"mock","label":null}"#
        );
        for absent in ["family", "capabilities", "when", "options"] {
            assert!(
                !json.contains(absent),
                "unexpected key {absent:?} in {json}"
            );
        }
    }

    #[test]
    fn deserializes_without_added_fields() {
        // Records written before this change (no added keys) still
        // deserialize, defaulting the new fields.
        let step: ModelLadderStepDef =
            serde_json::from_str(r#"{"model":"mock-cheap"}"#).expect("deserialize legacy");
        assert_eq!(step.when, None);
        assert_eq!(step.family, None);
        assert!(step.options.is_none());
        assert!(step.capabilities.is_empty());
    }

    #[test]
    fn catalog_toml_row_retains_when_and_options() {
        // A `[model_ladders.*]` catalog row carrying when/options/family/
        // capabilities parses WITHOUT silently discarding them — previously
        // these keys had no home on the DTO and were dropped on the floor.
        let toml_src = r#"
label = "with overrides"
steps = [
  { model = "haiku", label = "cheap", when = "transport_failure", family = "haiku", capabilities = ["tools"], options = { temperature = 0.1, max_tokens = 256 } },
  { model = "opus", label = "frontier", family = "opus" },
]
"#;
        let def: ModelLadderDef = toml::from_str(toml_src).expect("parse ladder toml");
        assert_eq!(def.steps.len(), 2);
        let cheap = &def.steps[0];
        assert_eq!(cheap.when.as_deref(), Some("transport_failure"));
        assert_eq!(cheap.family.as_deref(), Some("haiku"));
        assert_eq!(cheap.capabilities, vec!["tools".to_string()]);
        let opts = cheap.options.as_ref().expect("options present");
        assert_eq!(opts.get("temperature"), Some(&toml::Value::Float(0.1)));
        assert_eq!(opts.get("max_tokens"), Some(&toml::Value::Integer(256)));
        assert_eq!(def.steps[1].family.as_deref(), Some("opus"));
    }
}
