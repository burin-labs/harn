//! Model catalog DTOs: per-route serving definitions and the sub-records
//! (pricing, rate limits, serving performance, architecture, fast mode,
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

/// Optional accelerated-serving ("fast mode") tier for a model. Off by
/// default: its presence only *describes* that the provider offers a
/// faster, premium-priced serving path running the same weights — callers
/// must explicitly opt in via the provider's request knob, so nothing here
/// changes default behavior. Deliberately provider-agnostic: Anthropic
/// exposes the tier as `speed = "fast"` (beta-gated), while OpenAI uses
/// `service_tier = "fast"` / `"priority"`. Premium pricing is stored as
/// absolute per-MTok rates rather than a single multiplier because
/// providers price the tier asymmetrically (Anthropic Opus 4.8 is 2x
/// standard; Opus 4.7 fast mode is 6x).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct FastModeDef {
    /// Request field that opts into the fast tier (e.g. "speed" for
    /// Anthropic, "service_tier" for OpenAI).
    pub param: String,
    /// Value to send on `param` (e.g. "fast", "priority").
    pub value: String,
    /// Provider beta/feature header required to use the tier, if any
    /// (e.g. Anthropic "fast-mode-2026-02-01").
    #[serde(default)]
    pub beta_header: Option<String>,
    /// Output-tokens-per-second speedup vs standard serving (e.g. 2.5).
    #[serde(default)]
    pub otps_speedup: Option<f64>,
    /// Lifecycle of the fast tier: "ga" | "research_preview" |
    /// "deprecated". None when unspecified.
    #[serde(default)]
    pub status: Option<String>,
    /// Premium pricing charged while the fast tier is active (absolute
    /// per-MTok rates, not a multiplier on standard pricing).
    #[serde(default)]
    pub pricing: Option<ModelPricing>,
    /// Free-text note: constraints, deprecation timeline, etc.
    #[serde(default)]
    pub note: Option<String>,
}

/// A named model-fallback ladder declared in the catalog under
/// `[model_ladders.<name>]`. A `models`/`ladder` option on `llm_call`
/// lowers a ladder onto the first-class `routing_policy` chain: each step
/// is one transport attempt, and the loop advances to the next step ONLY
/// on transport-class failures (connection/timeout/429/5xx/throttled).
///
/// This data-driven declaration follows the same spirit as `fast_mode`
/// (#4017): a capability/behavior encoded as catalog data rather than
/// hand-rolled at each downstream call site (harn-bump-fleet,
/// harn-cloud free_tier_pool, burin-code all shipped their own copy).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelLadderDef {
    /// Ordered ladder steps, cheapest/first to most-capable/last.
    #[serde(default)]
    pub steps: Vec<ModelLadderStepDef>,
    /// Optional human-readable label surfaced on the routing envelope.
    #[serde(default)]
    pub label: Option<String>,
}

/// One rung of a [`ModelLadderDef`]. Mirrors the
/// `{model, provider?, label?, family?, capabilities?}` shape accepted by the
/// `models:` option and the `model_ladder(...)` std constructor. Provider is
/// optional: when omitted it is inferred from the model id (or the call's base
/// provider) at lowering time. `family`/`capabilities` are optional,
/// serde-absent-when-unset informational fields carried for downstream
/// selectors (e.g. harn-cloud free-tier routing); they do not affect Harn's
/// own transport failover.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelLadderStepDef {
    pub model: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
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
    /// Accelerated-serving ("fast mode") tier metadata, when the model's
    /// provider offers one. Off by default — see [`FastModeDef`]. None for
    /// models with no faster serving path.
    #[serde(default)]
    pub fast_mode: Option<FastModeDef>,
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
    use super::ModelLadderStepDef;

    #[test]
    fn family_and_capabilities_round_trip() {
        let step = ModelLadderStepDef {
            model: "claude-haiku-4-5".to_string(),
            provider: Some("anthropic".to_string()),
            label: Some("cheap".to_string()),
            family: Some("haiku".to_string()),
            capabilities: vec!["vision".to_string(), "tools".to_string()],
        };
        let json = serde_json::to_string(&step).expect("serialize");
        let back: ModelLadderStepDef = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(step, back);
        assert!(json.contains("\"family\":\"haiku\""));
        assert!(json.contains("\"capabilities\":[\"vision\",\"tools\"]"));
    }

    #[test]
    fn unset_family_and_capabilities_are_absent_from_serialized_output() {
        // A step that sets neither new field must serialize byte-identically
        // to the pre-existing {model, provider?, label?} shape: the new keys
        // are entirely absent (not `null`, not `[]`), so already-serialized
        // catalog bundles/records stay unchanged.
        let step = ModelLadderStepDef {
            model: "mock-cheap".to_string(),
            provider: Some("mock".to_string()),
            label: None,
            family: None,
            capabilities: Vec::new(),
        };
        let json = serde_json::to_string(&step).expect("serialize");
        assert_eq!(
            json,
            r#"{"model":"mock-cheap","provider":"mock","label":null}"#
        );
        assert!(!json.contains("family"));
        assert!(!json.contains("capabilities"));
    }

    #[test]
    fn deserializes_without_new_fields() {
        // Records written before this change (no family/capabilities keys)
        // still deserialize, defaulting the new fields.
        let step: ModelLadderStepDef =
            serde_json::from_str(r#"{"model":"mock-cheap"}"#).expect("deserialize legacy");
        assert_eq!(step.family, None);
        assert!(step.capabilities.is_empty());
    }
}
