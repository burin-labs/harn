//! Per-call provider telemetry envelope.
//!
//! Local runtimes (Ollama in particular) report enough server-side timing
//! information to diagnose slow runs without scraping daemon logs: cold load
//! vs steady state, prefill vs generation, and tokens-per-second ratios. The
//! Anthropic and OpenAI hosted APIs don't expose comparable metrics, but the
//! local runtimes Harn cares most about (Ollama, llama.cpp, MLX, vLLM) all
//! ship at least a partial subset. This module normalizes whatever the
//! provider reports into one stable envelope and represents missing fields
//! as `None` so downstream evals can distinguish "not reported" from "zero".
//!
//! Conventions:
//! - Durations are milliseconds (Ollama reports nanoseconds; we convert).
//! - Token counts are signed `i64` to match the rest of `LlmResult`.
//! - `source` identifies which wire format the values were lifted from so
//!   eval scripts can route on it without re-deriving provider names.

use crate::value::VmDictExt;

use crate::value::VmValue;

/// Wire-format identifiers for `ProviderTelemetry::source`. Keep these in
/// sync with the matching strings in `docs/src/observability/*` and the
/// The host eval aggregator.
pub mod source {
    /// Ollama `/api/chat` NDJSON stream — full timing breakdown.
    pub const OLLAMA_CHAT: &str = "ollama_chat";
    /// Ollama `/api/generate` (raw) — full timing breakdown.
    pub const OLLAMA_GENERATE: &str = "ollama_generate";
    /// OpenAI-style `usage` block (prompt/completion tokens, optional cache
    /// details). No server-side timings unless the runtime extends the
    /// schema.
    pub const OPENAI_USAGE: &str = "openai_usage";
    /// llama.cpp `timings` extension (`prompt_ms`, `predicted_ms`,
    /// `predicted_n`, `prompt_n`, ...). Preserved verbatim from the
    /// non-OpenAI subset.
    pub const LLAMACPP_TIMINGS: &str = "llamacpp_timings";
    /// Anthropic Messages API — usage counts only; no timings.
    pub const ANTHROPIC_USAGE: &str = "anthropic_usage";
    /// Google Gemini `usageMetadata` block from `generateContent`.
    pub const GEMINI_USAGE: &str = "gemini_usage";
    /// Google Gemini `usage` block from the Interactions API. Named apart from
    /// [`GEMINI_USAGE`] because the two families spell every counter
    /// differently, so eval joins can tell which endpoint served a call.
    pub const GEMINI_INTERACTIONS_USAGE: &str = "gemini_interactions_usage";
    /// Harn's deterministic mock/replay transport. No provider request occurs,
    /// so its authoritative provider spend is exactly zero even when the
    /// replay preserves the original provider/model identity.
    pub const MOCK_REPLAY: &str = "mock_replay";
    /// Provider responded but we did not capture anything beyond what
    /// already lives on `LlmResult` (e.g. mock / fake providers, or a
    /// stream that finished without a usage frame).
    pub const UNKNOWN: &str = "unknown";
}

pub(crate) fn elapsed_ms(started: std::time::Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

/// Provider-side timing and runtime accounting captured per LLM call.
///
/// All fields default to `None` / empty. Producers fill in what they can
/// extract and leave the rest absent; consumers must treat missing fields as
/// "not reported by this provider", not "zero".
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProviderTelemetry {
    /// Wire format the values came from (`ollama_chat`, `openai_usage`, ...).
    /// See [`source`] for the canonical strings. Empty when no telemetry was
    /// captured.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// Sanitized provider base URL used for this request. Credentials, query
    /// strings, and fragments are removed at the transport boundary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serving_base_url: Option<String>,
    /// Opaque identifier the server reported for the backend build or
    /// configuration that served this call (`system_fingerprint` on
    /// OpenAI-shaped responses; llama.cpp reports its build string there).
    ///
    /// This is a build discriminator, not host identity. It narrows — but
    /// does not close — the ambiguity left by
    /// [`serving_base_url`](Self::serving_base_url), which cannot separate
    /// several hosts serving byte-identical artifacts on the same local URL.
    /// It compares in one direction only: two different values prove two
    /// different server builds, while two equal values mean the servers
    /// agreed on a build, not that one of them served both calls.
    /// Attributing a call to a machine additionally needs an executing-host
    /// fact, which this envelope does not carry.
    ///
    /// Absent means the provider reported nothing — never "the same build as
    /// the last call".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serving_fingerprint: Option<String>,
    /// The provider block's `cache_usage_accounting` declaration for the
    /// route that served this call. `Some(true)`: the cache token fields are
    /// provider-audited and a zero is a real miss. `Some(false)`: the route
    /// reports no cache fields and the transport zeroes them deliberately.
    /// `None`: undeclared — parsed values are preserved as received, and a
    /// zero carries no information.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_accounting_declared: Option<bool>,
    /// Total server-side wall clock (Ollama `total_duration`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_total_ms: Option<u64>,
    /// Time the server spent loading/warming the model (Ollama
    /// `load_duration`). Useful for detecting cold-start latency.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_load_ms: Option<u64>,
    /// Prompt-prefill time (Ollama `prompt_eval_duration`). Anything else
    /// would be marketing — this is the field evals key on for prefill
    /// regression detection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_prompt_eval_ms: Option<u64>,
    /// Generation/decode time (Ollama `eval_duration`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_generation_ms: Option<u64>,
    /// Total prompt tokens reported by the provider's usage counter. Distinct
    /// from `LlmResult::input_tokens` because hosted providers frequently bill
    /// different token boundaries than the on-device tokenizer reports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_prompt_tokens: Option<i64>,
    /// Prompt tokens the server actually evaluated rather than reading from a
    /// cache. llama.cpp reports this as `timings.prompt_n`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_uncached_prompt_tokens: Option<i64>,
    /// Prompt tokens the server read from its prompt cache. llama.cpp reports
    /// this as `timings.cache_n`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_cached_prompt_tokens: Option<i64>,
    /// Tokens the server reports it generated (Ollama `eval_count`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_output_tokens: Option<i64>,
    /// Client-side wall clock around the HTTP request. Includes network and
    /// streaming latency the server-side counters omit. Recorded for every
    /// call regardless of provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_wall_ms: Option<u64>,
    /// Client-side latency from request dispatch to the first well-formed
    /// provider stream frame. Present only for streamed calls: a single-shot
    /// request has no first frame, so this stays absent rather than reporting
    /// zero, and a consumer can tell "not streamed" from "arrived instantly".
    /// Subtracting it from `client_wall_ms` separates the wait for the first
    /// frame from the time spent streaming the rest, on providers that report
    /// no server-side breakdown of their own. That subtraction is only sound
    /// because both are measured from the same origin in `transport`, and
    /// both therefore span the whole call: on a retried call each includes
    /// the attempts that failed, rather than starting over at the attempt
    /// that succeeded.
    ///
    /// Set only by the shared SSE and NDJSON readers. Provider paths with
    /// their own reader and their own `client_wall_ms` origin — Ollama's raw
    /// `/api/generate`, the OpenAI Responses path — leave this absent rather
    /// than reporting a value the sibling field cannot be differenced with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_first_frame_ms: Option<u64>,
    /// Context window the model was loaded with (where the runtime
    /// reports it; `/api/ps` for Ollama).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_context_length: Option<u64>,
    /// Exact model id the server resolved. May differ from
    /// `LlmResult::model` when an alias / digest is rewritten upstream.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_loaded_model: Option<String>,
    /// Model identity reported on the provider response. This is kept
    /// separate from the requested `LlmResult::model` so adapter and routing
    /// probes can fail closed when a server silently falls back.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    /// Total resident bytes for the loaded model (Ollama `/api/ps.size`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_memory_bytes: Option<u64>,
    /// VRAM bytes for the loaded model (Ollama `/api/ps.size_vram`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_memory_vram_bytes: Option<u64>,
    /// When the server will unload the model (Ollama `/api/ps.expires_at`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_keep_alive_until: Option<String>,
    /// Provider-supplied request id (`x-request-id` / `request_id`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Exact charge reported by the provider for this request. This takes
    /// precedence over catalog estimates when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_cost_usd: Option<f64>,
    /// Provider/router metadata returned alongside an otherwise standard wire
    /// response. Gateways use this for the resolved upstream, fallback
    /// attempts, routing policy, and exact billed cost. Preserved as JSON so
    /// Harn does not hard-code one router's schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<serde_json::Value>,
}

impl ProviderTelemetry {
    pub fn new(source: &str) -> Self {
        Self {
            source: source.to_string(),
            ..Self::default()
        }
    }

    pub(crate) fn mock_replay(simulated_cost_usd: Option<f64>) -> Self {
        Self {
            source: source::MOCK_REPLAY.to_string(),
            provider_metadata: simulated_cost_usd
                .map(|cost_usd| serde_json::json!({"harn_simulated_cost_usd": cost_usd})),
            ..Self::default()
        }
    }

    pub(crate) fn mock_replay_cost_usd(&self) -> Option<f64> {
        (self.source == source::MOCK_REPLAY).then(|| {
            self.provider_metadata
                .as_ref()
                .and_then(|metadata| metadata.get("harn_simulated_cost_usd"))
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0)
        })
    }

    /// Returns `true` when no meaningful telemetry was captured. A bare
    /// `client_wall_ms` is still "meaningful" — it lets evals reason about
    /// per-call latency even for providers that report nothing else.
    pub fn is_empty(&self) -> bool {
        let Self {
            source,
            serving_base_url,
            serving_fingerprint,
            cache_accounting_declared,
            server_total_ms,
            server_load_ms,
            server_prompt_eval_ms,
            server_generation_ms,
            server_prompt_tokens,
            server_uncached_prompt_tokens,
            server_cached_prompt_tokens,
            server_output_tokens,
            client_wall_ms,
            client_first_frame_ms,
            runtime_context_length,
            runtime_loaded_model,
            response_model,
            runtime_memory_bytes,
            runtime_memory_vram_bytes,
            runtime_keep_alive_until,
            request_id,
            provider_cost_usd,
            provider_metadata,
        } = self;
        source.is_empty()
            && serving_base_url.is_none()
            && serving_fingerprint.is_none()
            && cache_accounting_declared.is_none()
            && server_total_ms.is_none()
            && server_load_ms.is_none()
            && server_prompt_eval_ms.is_none()
            && server_generation_ms.is_none()
            && server_prompt_tokens.is_none()
            && server_uncached_prompt_tokens.is_none()
            && server_cached_prompt_tokens.is_none()
            && server_output_tokens.is_none()
            && client_wall_ms.is_none()
            && client_first_frame_ms.is_none()
            && runtime_context_length.is_none()
            && runtime_loaded_model.is_none()
            && response_model.is_none()
            && runtime_memory_bytes.is_none()
            && runtime_memory_vram_bytes.is_none()
            && runtime_keep_alive_until.is_none()
            && request_id.is_none()
            && provider_cost_usd.is_none()
            && provider_metadata.is_none()
    }

    /// Convert nanoseconds (Ollama's reporting unit) to milliseconds with
    /// integer rounding. Centralized so every Ollama timing field uses the
    /// same conversion and zero-vs-None semantics line up.
    pub fn ns_to_ms(ns: u64) -> u64 {
        // Use floor division (the conversion is approximate by design); when
        // the upstream reports 0 ns we want a 0 ms entry rather than None,
        // so callers should pass through `Some(ns_to_ms(0))` consciously.
        ns / 1_000_000
    }

    /// Extract Ollama-shape telemetry from a `done=true` chat or generate
    /// frame. Returns a populated [`ProviderTelemetry`] whose `source` is
    /// the caller-provided wire identifier.
    pub fn from_ollama_done(frame: &serde_json::Value, source: &str) -> Self {
        let mut telemetry = Self::new(source);
        telemetry.server_total_ms = ns_field(frame, "total_duration");
        telemetry.server_load_ms = ns_field(frame, "load_duration");
        telemetry.server_prompt_eval_ms = ns_field(frame, "prompt_eval_duration");
        telemetry.server_generation_ms = ns_field(frame, "eval_duration");
        telemetry.server_prompt_tokens = frame
            .get("prompt_eval_count")
            .and_then(serde_json::Value::as_i64);
        telemetry.server_output_tokens =
            frame.get("eval_count").and_then(serde_json::Value::as_i64);
        if let Some(model) = frame.get("model").and_then(serde_json::Value::as_str) {
            telemetry.runtime_loaded_model = Some(model.to_string());
        }
        telemetry
    }

    /// Extract telemetry from a complete OpenAI-shaped response or stream
    /// frame. llama.cpp puts `timings` beside `usage`; older adapters may put
    /// it inside `usage`, which remains a fallback.
    pub fn from_openai_response(response: &serde_json::Value, request_id: Option<&str>) -> Self {
        let usage = response.get("usage").unwrap_or(&serde_json::Value::Null);
        let mut telemetry = Self::new(source::OPENAI_USAGE);
        telemetry.server_prompt_tokens = usage
            .get("prompt_tokens")
            .or_else(|| usage.get("input_tokens"))
            .and_then(serde_json::Value::as_i64);
        telemetry.server_output_tokens = usage
            .get("completion_tokens")
            .or_else(|| usage.get("output_tokens"))
            .and_then(serde_json::Value::as_i64);
        if let Some(timings) = response
            .get("timings")
            .filter(|value| value.is_object())
            .or_else(|| usage.get("timings").filter(|value| value.is_object()))
        {
            telemetry.source = source::LLAMACPP_TIMINGS.to_string();
            telemetry.server_prompt_eval_ms = ms_or_round(timings.get("prompt_ms"));
            telemetry.server_generation_ms = ms_or_round(timings.get("predicted_ms"));
            telemetry.server_uncached_prompt_tokens =
                timings.get("prompt_n").and_then(serde_json::Value::as_i64);
            telemetry.server_cached_prompt_tokens =
                timings.get("cache_n").and_then(serde_json::Value::as_i64);
            if let Some(predicted) = timings
                .get("predicted_n")
                .and_then(serde_json::Value::as_i64)
            {
                telemetry.server_output_tokens = Some(predicted);
            }
            let total = telemetry
                .server_prompt_eval_ms
                .unwrap_or(0)
                .saturating_add(telemetry.server_generation_ms.unwrap_or(0));
            if total > 0 {
                telemetry.server_total_ms = Some(total);
            }
        }
        if let Some(request_id) = request_id.filter(|value| !value.is_empty()) {
            telemetry.request_id = Some(request_id.to_string());
        }
        let direct_cost_usd = usage
            .get("cost")
            .or_else(|| usage.get("total_cost"))
            .or_else(|| usage.get("estimated_cost"))
            .and_then(serde_json::Value::as_f64)
            .filter(|cost| cost.is_finite() && *cost >= 0.0);
        telemetry.provider_cost_usd = direct_cost_usd.or_else(|| {
            const XAI_USD_TICKS_PER_DOLLAR: f64 = 10_000_000_000.0;
            usage
                .get("cost_in_usd_ticks")
                .and_then(serde_json::Value::as_f64)
                .filter(|ticks| ticks.is_finite() && *ticks >= 0.0)
                .map(|ticks| ticks / XAI_USD_TICKS_PER_DOLLAR)
        });
        telemetry.capture_provider_metadata(response);
        telemetry
    }

    pub fn capture_request_id(&mut self, request_id: Option<&str>) {
        if self.request_id.is_none() {
            self.request_id = request_id
                .filter(|value| !value.is_empty())
                .map(str::to_string);
        }
    }

    /// Extract Anthropic-shape `usage` telemetry. Anthropic only reports
    /// input/output (and cache) counts — preserving the request id is the
    /// most useful incremental signal beyond what `LlmResult` already
    /// carries.
    pub fn from_anthropic_usage(usage: &serde_json::Value, request_id: Option<&str>) -> Self {
        let mut telemetry = Self::new(source::ANTHROPIC_USAGE);
        telemetry.server_prompt_tokens = usage
            .get("input_tokens")
            .and_then(serde_json::Value::as_i64);
        telemetry.server_output_tokens = usage
            .get("output_tokens")
            .and_then(serde_json::Value::as_i64);
        if let Some(request_id) = request_id.filter(|value| !value.is_empty()) {
            telemetry.request_id = Some(request_id.to_string());
        }
        telemetry
    }

    /// Extract Gemini `usageMetadata` counts. Cache-hit accounting stays on
    /// `LlmResult`; telemetry keeps provider prompt/output counters plus the
    /// request id for eval joins.
    pub fn from_gemini_usage(usage: &serde_json::Value, request_id: Option<&str>) -> Self {
        let mut telemetry = Self::new(source::GEMINI_USAGE);
        telemetry.server_prompt_tokens = usage
            .get("promptTokenCount")
            .and_then(serde_json::Value::as_i64);
        telemetry.server_output_tokens = usage
            .get("candidatesTokenCount")
            .and_then(serde_json::Value::as_i64);
        if let Some(request_id) = request_id.filter(|value| !value.is_empty()) {
            telemetry.request_id = Some(request_id.to_string());
        }
        telemetry
    }

    /// Extract Gemini Interactions `usage` counts. The Interactions envelope
    /// spells prompt/output counts as `total_input_tokens` /
    /// `total_output_tokens` and carries the interaction id (the handle a
    /// follow-up turn chains from) as `id`.
    pub fn from_gemini_interactions_usage(
        usage: &serde_json::Value,
        request_id: Option<&str>,
    ) -> Self {
        let mut telemetry = Self::new(source::GEMINI_INTERACTIONS_USAGE);
        telemetry.server_prompt_tokens = usage
            .get("total_input_tokens")
            .and_then(serde_json::Value::as_i64);
        telemetry.server_output_tokens = usage
            .get("total_output_tokens")
            .and_then(serde_json::Value::as_i64);
        if let Some(request_id) = request_id.filter(|value| !value.is_empty()) {
            telemetry.request_id = Some(request_id.to_string());
        }
        telemetry
    }

    /// Preserve a non-empty top-level `provider_metadata` object. OpenAI-style
    /// gateways put it on both non-streaming responses and the final SSE frame.
    pub fn capture_provider_metadata(&mut self, response: &serde_json::Value) {
        if let Some(model) = response
            .get("model")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
        {
            self.response_model = Some(model.to_string());
        }
        if let Some(fingerprint) = response
            .get("system_fingerprint")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
        {
            self.serving_fingerprint = Some(fingerprint.to_string());
        }
        if let Some(metadata) = response
            .get("provider_metadata")
            .filter(|value| !value.is_null())
            .filter(|value| !value.as_object().is_some_and(serde_json::Map::is_empty))
        {
            merge_provider_metadata(&mut self.provider_metadata, metadata);
        }
        if let Some(receipt) = response
            .get(crate::llm::managed_supply::MANAGED_SUPPLY_WIRE_KEY)
            .filter(|value| !value.is_null())
        {
            let mut managed = serde_json::Map::new();
            managed.insert(
                crate::llm::managed_supply::MANAGED_SUPPLY_WIRE_KEY.to_string(),
                receipt.clone(),
            );
            merge_provider_metadata(
                &mut self.provider_metadata,
                &serde_json::Value::Object(managed),
            );
        }
    }

    /// Merge a `/api/ps` snapshot of a loaded Ollama model into this
    /// telemetry envelope. Only fills in fields that were not already
    /// populated so a per-call extraction keeps precedence.
    pub fn merge_ollama_ps(&mut self, ps: &OllamaPsModel) {
        if self.runtime_loaded_model.is_none() {
            self.runtime_loaded_model = ps.name.clone();
        }
        if self.runtime_context_length.is_none() {
            self.runtime_context_length = ps.context_length;
        }
        if self.runtime_memory_bytes.is_none() {
            self.runtime_memory_bytes = ps.size_bytes;
        }
        if self.runtime_memory_vram_bytes.is_none() {
            self.runtime_memory_vram_bytes = ps.size_vram_bytes;
        }
        if self.runtime_keep_alive_until.is_none() {
            self.runtime_keep_alive_until = ps.expires_at.clone();
        }
    }

    /// Render this envelope into the dictionary shape `llm_call` returns.
    /// Returns `None` if the envelope is empty so callers can omit the key
    /// entirely.
    pub fn as_vm_dict(&self) -> Option<VmValue> {
        if self.is_empty() {
            return None;
        }
        let mut dict: crate::value::DictMap = crate::value::DictMap::new();
        if !self.source.is_empty() {
            dict.put_str("source", self.source.as_str());
        }
        if let Some(ref serving_base_url) = self.serving_base_url {
            dict.put_str("serving_base_url", serving_base_url.as_str());
        }
        if let Some(ref serving_fingerprint) = self.serving_fingerprint {
            dict.put_str("serving_fingerprint", serving_fingerprint.as_str());
        }
        insert_opt_u64(&mut dict, "server_total_ms", self.server_total_ms);
        insert_opt_u64(&mut dict, "server_load_ms", self.server_load_ms);
        insert_opt_u64(
            &mut dict,
            "server_prompt_eval_ms",
            self.server_prompt_eval_ms,
        );
        insert_opt_u64(&mut dict, "server_generation_ms", self.server_generation_ms);
        insert_opt_i64(&mut dict, "server_prompt_tokens", self.server_prompt_tokens);
        insert_opt_i64(
            &mut dict,
            "server_uncached_prompt_tokens",
            self.server_uncached_prompt_tokens,
        );
        insert_opt_i64(
            &mut dict,
            "server_cached_prompt_tokens",
            self.server_cached_prompt_tokens,
        );
        insert_opt_i64(&mut dict, "server_output_tokens", self.server_output_tokens);
        insert_opt_u64(&mut dict, "client_wall_ms", self.client_wall_ms);
        insert_opt_u64(
            &mut dict,
            "client_first_frame_ms",
            self.client_first_frame_ms,
        );
        insert_opt_u64(
            &mut dict,
            "runtime_context_length",
            self.runtime_context_length,
        );
        if let Some(ref model) = self.runtime_loaded_model {
            dict.put_str("runtime_loaded_model", model.as_str());
        }
        if let Some(ref model) = self.response_model {
            dict.put_str("response_model", model.as_str());
        }
        insert_opt_u64(&mut dict, "runtime_memory_bytes", self.runtime_memory_bytes);
        insert_opt_u64(
            &mut dict,
            "runtime_memory_vram_bytes",
            self.runtime_memory_vram_bytes,
        );
        if let Some(ref expires) = self.runtime_keep_alive_until {
            dict.put_str("runtime_keep_alive_until", expires.as_str());
        }
        if let Some(ref request_id) = self.request_id {
            dict.put_str("request_id", request_id.as_str());
        }
        if let Some(provider_cost_usd) = self.provider_cost_usd {
            dict.insert(
                crate::value::intern_key("provider_cost_usd"),
                VmValue::Float(provider_cost_usd),
            );
        }
        if let Some(ref provider_metadata) = self.provider_metadata {
            dict.insert(
                crate::value::intern_key("provider_metadata"),
                crate::stdlib::json_to_vm_value(provider_metadata),
            );
        }
        Some(VmValue::dict(dict))
    }
}

fn merge_provider_metadata(target: &mut Option<serde_json::Value>, incoming: &serde_json::Value) {
    let Some(incoming) = incoming.as_object() else {
        return;
    };
    let target = target.get_or_insert_with(|| serde_json::Value::Object(Default::default()));
    let Some(target) = target.as_object_mut() else {
        return;
    };
    for (key, value) in incoming {
        target.insert(key.clone(), value.clone());
    }
}

/// Loaded-model snapshot from Ollama's `/api/ps`. Shared with the CLI's
/// `harn local` family so we don't duplicate the response shape.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OllamaPsModel {
    pub name: Option<String>,
    pub size_bytes: Option<u64>,
    pub size_vram_bytes: Option<u64>,
    pub expires_at: Option<String>,
    pub context_length: Option<u64>,
}

impl OllamaPsModel {
    /// Decode one `/api/ps` `models[]` entry. Returns `None` when the entry
    /// has no usable identifier (an old daemon may emit completely empty
    /// rows under load).
    pub fn from_ps_entry(entry: &serde_json::Value) -> Option<Self> {
        let name = entry
            .get("name")
            .and_then(serde_json::Value::as_str)
            .or_else(|| entry.get("model").and_then(serde_json::Value::as_str))
            .map(str::to_string);
        let context_length = entry
            .get("context_length")
            .and_then(serde_json::Value::as_u64)
            .or_else(|| {
                entry
                    .get("details")
                    .and_then(|details| details.get("context_length"))
                    .and_then(serde_json::Value::as_u64)
            });
        Some(Self {
            name,
            size_bytes: entry.get("size").and_then(serde_json::Value::as_u64),
            size_vram_bytes: entry.get("size_vram").and_then(serde_json::Value::as_u64),
            expires_at: entry
                .get("expires_at")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            context_length,
        })
    }
}

fn ns_field(frame: &serde_json::Value, key: &str) -> Option<u64> {
    frame
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .map(ProviderTelemetry::ns_to_ms)
}

fn ms_or_round(value: Option<&serde_json::Value>) -> Option<u64> {
    let value = value?;
    if let Some(n) = value.as_u64() {
        return Some(n);
    }
    value.as_f64().map(|n| n.round().max(0.0) as u64)
}

fn insert_opt_u64(dict: &mut crate::value::DictMap, key: &str, value: Option<u64>) {
    if let Some(value) = value {
        dict.insert(crate::value::intern_key(key), VmValue::Int(value as i64));
    }
}

fn insert_opt_i64(dict: &mut crate::value::DictMap, key: &str, value: Option<i64>) {
    if let Some(value) = value {
        dict.insert(crate::value::intern_key(key), VmValue::Int(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ollama_done_frame_extracts_full_breakdown() {
        let frame = serde_json::json!({
            "model": "devstral-small-2:24b",
            "total_duration": 7_400_000_000u64,
            "load_duration": 400_000_000u64,
            "prompt_eval_duration": 1_200_000_000u64,
            "eval_duration": 5_800_000_000u64,
            "prompt_eval_count": 1024,
            "eval_count": 64
        });

        let telemetry = ProviderTelemetry::from_ollama_done(&frame, source::OLLAMA_CHAT);

        assert_eq!(telemetry.source, source::OLLAMA_CHAT);
        assert_eq!(telemetry.server_total_ms, Some(7400));
        assert_eq!(telemetry.server_load_ms, Some(400));
        assert_eq!(telemetry.server_prompt_eval_ms, Some(1200));
        assert_eq!(telemetry.server_generation_ms, Some(5800));
        assert_eq!(telemetry.server_prompt_tokens, Some(1024));
        assert_eq!(telemetry.server_output_tokens, Some(64));
        assert_eq!(
            telemetry.runtime_loaded_model.as_deref(),
            Some("devstral-small-2:24b")
        );
        assert!(!telemetry.is_empty());
    }

    #[test]
    fn ollama_done_frame_leaves_missing_fields_as_none() {
        let frame = serde_json::json!({
            "model": "devstral-small-2:24b",
            // Older Ollama builds omit duration fields on early frames.
        });

        let telemetry = ProviderTelemetry::from_ollama_done(&frame, source::OLLAMA_CHAT);

        assert_eq!(telemetry.server_total_ms, None);
        assert_eq!(telemetry.server_load_ms, None);
        assert_eq!(telemetry.server_prompt_eval_ms, None);
        assert_eq!(telemetry.server_generation_ms, None);
        assert_eq!(telemetry.server_prompt_tokens, None);
        assert_eq!(telemetry.server_output_tokens, None);
    }

    #[test]
    fn openai_usage_extracts_counts_and_provider_cost() {
        let response = serde_json::json!({
            "usage": {
                "prompt_tokens": 200,
                "completion_tokens": 50,
                "cost": 0.00125
            }
        });

        let telemetry = ProviderTelemetry::from_openai_response(&response, Some("req-abc"));

        assert_eq!(telemetry.source, source::OPENAI_USAGE);
        assert_eq!(telemetry.server_prompt_tokens, Some(200));
        assert_eq!(telemetry.server_output_tokens, Some(50));
        assert_eq!(telemetry.server_prompt_eval_ms, None);
        assert_eq!(telemetry.request_id.as_deref(), Some("req-abc"));
        assert_eq!(telemetry.provider_cost_usd, Some(0.00125));
    }

    #[test]
    fn openai_usage_cost_names_keep_precedence_and_validate_estimates() {
        let all_names = serde_json::json!({
            "usage": {
                "cost": 0.001,
                "total_cost": 0.002,
                "estimated_cost": 0.003
            }
        });
        let total_and_estimate = serde_json::json!({
            "usage": {
                "total_cost": 0.002,
                "estimated_cost": 0.003
            }
        });
        let estimate_only = serde_json::json!({
            "usage": { "estimated_cost": 0.003 }
        });

        assert_eq!(
            ProviderTelemetry::from_openai_response(&all_names, None).provider_cost_usd,
            Some(0.001)
        );
        assert_eq!(
            ProviderTelemetry::from_openai_response(&total_and_estimate, None).provider_cost_usd,
            Some(0.002)
        );
        assert_eq!(
            ProviderTelemetry::from_openai_response(&estimate_only, None).provider_cost_usd,
            Some(0.003)
        );

        for invalid in [serde_json::json!(-0.003), serde_json::json!("0.003")] {
            let response = serde_json::json!({
                "usage": { "estimated_cost": invalid }
            });
            assert_eq!(
                ProviderTelemetry::from_openai_response(&response, None).provider_cost_usd,
                None
            );
        }
    }

    #[test]
    fn openai_usage_converts_xai_cost_ticks_after_direct_cost_fields() {
        let ticks_only = serde_json::json!({
            "usage": { "cost_in_usd_ticks": 3_546_000 }
        });
        let direct_and_ticks = serde_json::json!({
            "usage": {
                "estimated_cost": 0.0002736,
                "cost_in_usd_ticks": 3_546_000
            }
        });

        assert_eq!(
            ProviderTelemetry::from_openai_response(&ticks_only, None).provider_cost_usd,
            Some(0.0003546)
        );
        assert_eq!(
            ProviderTelemetry::from_openai_response(&direct_and_ticks, None).provider_cost_usd,
            Some(0.0002736)
        );
        assert_eq!(
            ProviderTelemetry::from_openai_response(
                &serde_json::json!({"usage": {"cost_in_usd_ticks": 0}}),
                None,
            )
            .provider_cost_usd,
            Some(0.0)
        );

        for invalid in [
            serde_json::json!(-3_546_000),
            serde_json::json!("3546000"),
            serde_json::Value::Null,
        ] {
            let response = serde_json::json!({
                "usage": { "cost_in_usd_ticks": invalid }
            });
            assert_eq!(
                ProviderTelemetry::from_openai_response(&response, None).provider_cost_usd,
                None
            );
        }
    }

    #[test]
    fn nested_llamacpp_timings_remain_a_fallback() {
        let response = serde_json::json!({
            "usage": {
                "prompt_tokens": 220,
                "completion_tokens": 17,
                "timings": {
                    "prompt_n": 200,
                    "cache_n": 20,
                    "prompt_ms": 145.4,
                    "predicted_n": 17,
                    "predicted_ms": 89.1
                }
            }
        });

        let telemetry = ProviderTelemetry::from_openai_response(&response, None);

        assert_eq!(telemetry.source, source::LLAMACPP_TIMINGS);
        assert_eq!(telemetry.server_prompt_eval_ms, Some(145));
        assert_eq!(telemetry.server_generation_ms, Some(89));
        assert_eq!(telemetry.server_total_ms, Some(234));
        assert_eq!(telemetry.server_prompt_tokens, Some(220));
        assert_eq!(telemetry.server_uncached_prompt_tokens, Some(200));
        assert_eq!(telemetry.server_cached_prompt_tokens, Some(20));
        assert_eq!(telemetry.server_output_tokens, Some(17));
        assert!(!telemetry.is_empty());
    }

    #[test]
    fn ps_entry_pulls_context_length_from_top_level_or_details() {
        let entry = serde_json::json!({
            "name": "devstral-small-2:24b",
            "size": 4_700_000_000u64,
            "size_vram": 4_500_000_000u64,
            "expires_at": "2026-05-14T10:30:00Z",
            "context_length": 32768
        });
        let model = OllamaPsModel::from_ps_entry(&entry).expect("ps entry parses");
        assert_eq!(model.context_length, Some(32768));

        let entry_nested = serde_json::json!({
            "name": "devstral-small-2:24b",
            "details": {"context_length": 16384}
        });
        let nested = OllamaPsModel::from_ps_entry(&entry_nested).expect("ps entry parses");
        assert_eq!(nested.context_length, Some(16384));
    }

    #[test]
    fn merge_ollama_ps_preserves_call_level_values() {
        let mut telemetry = ProviderTelemetry::new(source::OLLAMA_CHAT);
        telemetry.runtime_loaded_model = Some("real-model".to_string());
        let ps = OllamaPsModel {
            name: Some("alias-model".to_string()),
            size_bytes: Some(1),
            size_vram_bytes: Some(2),
            expires_at: Some("forever".to_string()),
            context_length: Some(8192),
        };
        telemetry.merge_ollama_ps(&ps);
        assert_eq!(
            telemetry.runtime_loaded_model.as_deref(),
            Some("real-model")
        );
        assert_eq!(telemetry.runtime_memory_bytes, Some(1));
        assert_eq!(telemetry.runtime_memory_vram_bytes, Some(2));
        assert_eq!(
            telemetry.runtime_keep_alive_until.as_deref(),
            Some("forever")
        );
        assert_eq!(telemetry.runtime_context_length, Some(8192));
    }

    #[test]
    fn as_vm_dict_returns_none_when_empty() {
        let telemetry = ProviderTelemetry::default();
        assert!(telemetry.is_empty());
        assert!(telemetry.as_vm_dict().is_none());
    }

    #[test]
    fn as_vm_dict_serializes_all_present_fields() {
        let telemetry = ProviderTelemetry {
            source: source::OLLAMA_CHAT.to_string(),
            serving_base_url: Some("https://provider.example/v1".to_string()),
            server_total_ms: Some(100),
            client_wall_ms: Some(120),
            runtime_loaded_model: Some("qwen".to_string()),
            ..Default::default()
        };
        let value = telemetry.as_vm_dict().expect("dict present");
        let dict = value.as_dict().expect("dict body");
        assert_eq!(
            dict.get("source").map(VmValue::display).as_deref(),
            Some(source::OLLAMA_CHAT)
        );
        assert_eq!(
            dict.get("serving_base_url")
                .map(VmValue::display)
                .as_deref(),
            Some("https://provider.example/v1")
        );
        assert_eq!(
            dict.get("server_total_ms").and_then(|v| match v {
                VmValue::Int(n) => Some(*n),
                _ => None,
            }),
            Some(100)
        );
        assert_eq!(
            dict.get("client_wall_ms").and_then(|v| match v {
                VmValue::Int(n) => Some(*n),
                _ => None,
            }),
            Some(120)
        );
    }

    /// Fields that reach the transcript artifact but are deliberately not
    /// projected into the VM dict, with the reason each one is exempt. Keep
    /// this list short and justified: every entry is a field a `.harn` script
    /// cannot read.
    ///
    /// - `cache_accounting_declared` is consumed in Rust by
    ///   `crate::llm::usage`, which folds it into the derived `cache_hit_ratio`
    ///   a script actually reads. Projecting the raw declaration alongside the
    ///   derived value would give the same question two answers.
    const RUST_INTERNAL_TELEMETRY_FIELDS: &[&str] = &["cache_accounting_declared"];

    /// `as_vm_dict` is a hand-maintained whitelist, not a derive. A field added
    /// to the struct reaches `llm_transcript.jsonl` for free through
    /// whole-struct serde, while staying invisible to every `.harn` script that
    /// reads `usage.provider_telemetry` until someone remembers to list it. The
    /// failure is quiet in the worst way: the field looks wired because the
    /// artifact has it, and reads as absent everywhere a script looks.
    ///
    /// The neighbouring `as_vm_dict_serializes_all_present_fields` does not
    /// catch this despite its name — it spot-checks four fields. This is the
    /// census: populate every field, then require each serialized key to
    /// survive the projection.
    #[test]
    fn as_vm_dict_projects_every_serialized_field() {
        let telemetry = ProviderTelemetry {
            source: source::OLLAMA_CHAT.to_string(),
            serving_base_url: Some("https://provider.example/v1".to_string()),
            serving_fingerprint: Some("build-1".to_string()),
            cache_accounting_declared: Some(true),
            server_total_ms: Some(100),
            server_load_ms: Some(1),
            server_prompt_eval_ms: Some(2),
            server_generation_ms: Some(3),
            server_prompt_tokens: Some(4),
            server_uncached_prompt_tokens: Some(2),
            server_cached_prompt_tokens: Some(2),
            server_output_tokens: Some(5),
            client_wall_ms: Some(2_000),
            client_first_frame_ms: Some(1_500),
            runtime_context_length: Some(8_192),
            runtime_loaded_model: Some("served-model".to_string()),
            response_model: Some("response-model".to_string()),
            runtime_memory_bytes: Some(6),
            runtime_memory_vram_bytes: Some(7),
            runtime_keep_alive_until: Some("2026-01-01T00:00:00Z".to_string()),
            request_id: Some("req-1".to_string()),
            provider_cost_usd: Some(0.5),
            provider_metadata: Some(serde_json::json!({"tier": "standard"})),
        };

        let encoded = serde_json::to_value(&telemetry).expect("telemetry serializes");
        let encoded = encoded.as_object().expect("telemetry is a JSON object");
        let value = telemetry.as_vm_dict().expect("dict present");
        let dict = value.as_dict().expect("dict body");

        for key in encoded.keys() {
            if RUST_INTERNAL_TELEMETRY_FIELDS.contains(&key.as_str()) {
                continue;
            }
            assert!(
                dict.get(key.as_str()).is_some(),
                "{key} reaches the transcript but not the VM dict: add it to \
                 as_vm_dict, or to RUST_INTERNAL_TELEMETRY_FIELDS with a reason"
            );
        }

        // Expire a stale exemption: if an internal field ever does get
        // projected, this list must shrink rather than quietly over-claim.
        for exempt in RUST_INTERNAL_TELEMETRY_FIELDS {
            assert!(
                dict.get(*exempt).is_none(),
                "{exempt} is now projected; drop it from \
                 RUST_INTERNAL_TELEMETRY_FIELDS"
            );
        }

        // The census is only meaningful if the struct really was fully
        // populated; a field left at `None` would be skipped by serde and
        // silently excused from the check above.
        assert_eq!(
            encoded.len(),
            23,
            "every ProviderTelemetry field must be populated for the census to \
             cover it; update this count when the struct gains a field"
        );
    }

    /// An unmeasured first frame stays absent through both projections, so a
    /// single-shot call cannot read as one that answered instantly.
    #[test]
    fn an_absent_first_frame_is_omitted_rather_than_zeroed() {
        let telemetry = ProviderTelemetry {
            source: source::OLLAMA_CHAT.to_string(),
            client_wall_ms: Some(2_000),
            ..Default::default()
        };
        let value = telemetry.as_vm_dict().expect("dict present");
        let dict = value.as_dict().expect("dict body");
        assert!(
            dict.get("client_first_frame_ms").is_none(),
            "absent must not project as 0 into the VM dict"
        );
        let encoded = serde_json::to_value(&telemetry).expect("telemetry serializes");
        assert!(
            encoded.get("client_first_frame_ms").is_none(),
            "absent must not serialize as 0 into the artifact"
        );
    }

    #[test]
    fn gateway_provider_metadata_is_preserved_without_schema_coupling() {
        let response = serde_json::json!({
            "model": "served-adapter",
            "provider_metadata": {
                "gateway": {
                    "routing": {
                        "resolvedProvider": "openai",
                        "modelAttemptCount": 1
                    },
                    "cost": "0.00003865"
                }
            }
        });
        let mut telemetry = ProviderTelemetry::default();
        telemetry.capture_provider_metadata(&response);

        assert_eq!(
            telemetry
                .provider_metadata
                .as_ref()
                .and_then(|metadata| metadata.pointer("/gateway/routing/resolvedProvider"))
                .and_then(serde_json::Value::as_str),
            Some("openai")
        );
        assert_eq!(telemetry.response_model.as_deref(), Some("served-adapter"));
        assert!(!telemetry.is_empty());
        let value = telemetry
            .as_vm_dict()
            .expect("metadata makes telemetry visible");
        let dict = value.as_dict().expect("dict body");
        assert!(dict.get("provider_metadata").is_some());
        assert_eq!(
            dict.get("response_model").map(VmValue::display).as_deref(),
            Some("served-adapter")
        );
    }

    #[test]
    fn system_fingerprint_is_captured_and_projected() {
        let response = serde_json::json!({
            "model": "served-adapter",
            "system_fingerprint": "b10360-48d22e295"
        });
        let mut telemetry = ProviderTelemetry::default();
        telemetry.capture_provider_metadata(&response);

        assert_eq!(
            telemetry.serving_fingerprint.as_deref(),
            Some("b10360-48d22e295")
        );
        let value = telemetry
            .as_vm_dict()
            .expect("fingerprint makes telemetry visible");
        let dict = value.as_dict().expect("dict body");
        assert_eq!(
            dict.get("serving_fingerprint")
                .map(VmValue::display)
                .as_deref(),
            Some("b10360-48d22e295")
        );
    }

    #[test]
    fn absent_system_fingerprint_stays_absent() {
        // A server that reports no fingerprint must not read as one that
        // reported an empty build id: "" would compare equal across two
        // different hosts and silently re-create the ambiguity this field
        // exists to remove.
        let response = serde_json::json!({"model": "served-adapter"});
        let mut telemetry = ProviderTelemetry::default();
        telemetry.capture_provider_metadata(&response);

        assert_eq!(telemetry.serving_fingerprint, None);
        let value = telemetry.as_vm_dict().expect("model keeps telemetry alive");
        let dict = value.as_dict().expect("dict body");
        assert!(dict.get("serving_fingerprint").is_none());
    }

    #[test]
    fn empty_system_fingerprint_does_not_overwrite_a_reported_build() {
        let mut telemetry = ProviderTelemetry::default();
        telemetry.capture_provider_metadata(&serde_json::json!({
            "system_fingerprint": "b10360-48d22e295"
        }));
        telemetry.capture_provider_metadata(&serde_json::json!({
            "system_fingerprint": ""
        }));

        assert_eq!(
            telemetry.serving_fingerprint.as_deref(),
            Some("b10360-48d22e295")
        );
    }

    #[test]
    fn fingerprint_alone_makes_the_envelope_non_empty() {
        // `is_empty` gates `as_vm_dict`; if the new field were left out of
        // that destructure a fingerprint-only envelope would serialize to
        // nothing and the discriminator would never reach a run record.
        let telemetry = ProviderTelemetry {
            serving_fingerprint: Some("b10360-48d22e295".to_string()),
            ..ProviderTelemetry::default()
        };
        assert!(!telemetry.is_empty());
        assert!(telemetry.as_vm_dict().is_some());
    }
}
