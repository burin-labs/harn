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

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::VmValue;

/// Wire-format identifiers for `ProviderTelemetry::source`. Keep these in
/// sync with the matching strings in `docs/src/observability/*` and the
/// Burin eval aggregator.
pub mod source {
    /// Ollama `/api/chat` NDJSON stream — full timing breakdown.
    pub const OLLAMA_CHAT: &str = "ollama_chat";
    /// Ollama `/api/generate` (raw) — full timing breakdown.
    pub const OLLAMA_GENERATE: &str = "ollama_generate";
    /// OpenAI-style `usage` block (prompt/completion tokens, optional cache
    /// details). No server-side timings unless the runtime extends the
    /// schema.
    pub const OPENAI_USAGE: &str = "openai_usage";
    /// llama.cpp `usage.timings` extension (`prompt_ms`, `predicted_ms`,
    /// `predicted_n`, `prompt_n`, ...). Preserved verbatim from the
    /// non-OpenAI subset.
    pub const LLAMACPP_TIMINGS: &str = "llamacpp_timings";
    /// Anthropic Messages API — usage counts only; no timings.
    pub const ANTHROPIC_USAGE: &str = "anthropic_usage";
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
    /// Tokens the server reports it prefilled (Ollama `prompt_eval_count`).
    /// Distinct from `LlmResult::input_tokens` because hosted providers
    /// frequently bill different token boundaries than the on-device
    /// tokenizer reports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_prompt_tokens: Option<i64>,
    /// Tokens the server reports it generated (Ollama `eval_count`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_output_tokens: Option<i64>,
    /// Client-side wall clock around the HTTP request. Includes network and
    /// streaming latency the server-side counters omit. Recorded for every
    /// call regardless of provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_wall_ms: Option<u64>,
    /// Context window the model was loaded with (where the runtime
    /// reports it; `/api/ps` for Ollama).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_context_length: Option<u64>,
    /// Exact model id the server resolved. May differ from
    /// `LlmResult::model` when an alias / digest is rewritten upstream.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_loaded_model: Option<String>,
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
}

impl ProviderTelemetry {
    pub fn new(source: &str) -> Self {
        Self {
            source: source.to_string(),
            ..Self::default()
        }
    }

    /// Returns `true` when no meaningful telemetry was captured. A bare
    /// `client_wall_ms` is still "meaningful" — it lets evals reason about
    /// per-call latency even for providers that report nothing else.
    pub fn is_empty(&self) -> bool {
        let Self {
            source,
            server_total_ms,
            server_load_ms,
            server_prompt_eval_ms,
            server_generation_ms,
            server_prompt_tokens,
            server_output_tokens,
            client_wall_ms,
            runtime_context_length,
            runtime_loaded_model,
            runtime_memory_bytes,
            runtime_memory_vram_bytes,
            runtime_keep_alive_until,
            request_id,
        } = self;
        source.is_empty()
            && server_total_ms.is_none()
            && server_load_ms.is_none()
            && server_prompt_eval_ms.is_none()
            && server_generation_ms.is_none()
            && server_prompt_tokens.is_none()
            && server_output_tokens.is_none()
            && client_wall_ms.is_none()
            && runtime_context_length.is_none()
            && runtime_loaded_model.is_none()
            && runtime_memory_bytes.is_none()
            && runtime_memory_vram_bytes.is_none()
            && runtime_keep_alive_until.is_none()
            && request_id.is_none()
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

    /// Extract OpenAI-shape `usage` telemetry. Most OpenAI-compatible local
    /// runtimes only report counts; llama.cpp's `usage.timings` extension is
    /// folded in here as well so a single envelope captures both shapes.
    pub fn from_openai_usage(usage: &serde_json::Value, request_id: Option<&str>) -> Self {
        let mut telemetry = Self::new(source::OPENAI_USAGE);
        telemetry.server_prompt_tokens = usage
            .get("prompt_tokens")
            .and_then(serde_json::Value::as_i64);
        telemetry.server_output_tokens = usage
            .get("completion_tokens")
            .and_then(serde_json::Value::as_i64);
        if let Some(timings) = usage.get("timings").filter(|value| value.is_object()) {
            telemetry.source = source::LLAMACPP_TIMINGS.to_string();
            telemetry.server_prompt_eval_ms = ms_or_round(timings.get("prompt_ms"));
            telemetry.server_generation_ms = ms_or_round(timings.get("predicted_ms"));
            // llama.cpp also reports `prompt_n` / `predicted_n` here when
            // its usage breakdown is enabled; prefer them over the
            // legacy top-level counts so prefill cache hits surface
            // correctly.
            if let Some(prefill) = timings.get("prompt_n").and_then(serde_json::Value::as_i64) {
                telemetry.server_prompt_tokens = Some(prefill);
            }
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
        telemetry
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
        let mut dict: BTreeMap<String, VmValue> = BTreeMap::new();
        if !self.source.is_empty() {
            dict.insert(
                "source".to_string(),
                VmValue::String(Rc::from(self.source.as_str())),
            );
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
        insert_opt_i64(&mut dict, "server_output_tokens", self.server_output_tokens);
        insert_opt_u64(&mut dict, "client_wall_ms", self.client_wall_ms);
        insert_opt_u64(
            &mut dict,
            "runtime_context_length",
            self.runtime_context_length,
        );
        if let Some(ref model) = self.runtime_loaded_model {
            dict.insert(
                "runtime_loaded_model".to_string(),
                VmValue::String(Rc::from(model.as_str())),
            );
        }
        insert_opt_u64(&mut dict, "runtime_memory_bytes", self.runtime_memory_bytes);
        insert_opt_u64(
            &mut dict,
            "runtime_memory_vram_bytes",
            self.runtime_memory_vram_bytes,
        );
        if let Some(ref expires) = self.runtime_keep_alive_until {
            dict.insert(
                "runtime_keep_alive_until".to_string(),
                VmValue::String(Rc::from(expires.as_str())),
            );
        }
        if let Some(ref request_id) = self.request_id {
            dict.insert(
                "request_id".to_string(),
                VmValue::String(Rc::from(request_id.as_str())),
            );
        }
        Some(VmValue::Dict(Rc::new(dict)))
    }
}

/// Loaded-model snapshot from Ollama's `/api/ps`. Shared with the CLI's
/// `harn local` family so we don't duplicate the response shape.
#[derive(Clone, Debug, Default, PartialEq)]
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

fn insert_opt_u64(dict: &mut BTreeMap<String, VmValue>, key: &str, value: Option<u64>) {
    if let Some(value) = value {
        dict.insert(key.to_string(), VmValue::Int(value as i64));
    }
}

fn insert_opt_i64(dict: &mut BTreeMap<String, VmValue>, key: &str, value: Option<i64>) {
    if let Some(value) = value {
        dict.insert(key.to_string(), VmValue::Int(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ollama_done_frame_extracts_full_breakdown() {
        let frame = serde_json::json!({
            "model": "qwen3.6:35b-a3b-coding-nvfp4",
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
            Some("qwen3.6:35b-a3b-coding-nvfp4")
        );
        assert!(!telemetry.is_empty());
    }

    #[test]
    fn ollama_done_frame_leaves_missing_fields_as_none() {
        let frame = serde_json::json!({
            "model": "qwen3.6:35b-a3b-coding-nvfp4",
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
    fn openai_usage_partial_extracts_counts_only() {
        let usage = serde_json::json!({
            "prompt_tokens": 200,
            "completion_tokens": 50
        });

        let telemetry = ProviderTelemetry::from_openai_usage(&usage, Some("req-abc"));

        assert_eq!(telemetry.source, source::OPENAI_USAGE);
        assert_eq!(telemetry.server_prompt_tokens, Some(200));
        assert_eq!(telemetry.server_output_tokens, Some(50));
        assert_eq!(telemetry.server_prompt_eval_ms, None);
        assert_eq!(telemetry.request_id.as_deref(), Some("req-abc"));
    }

    #[test]
    fn llamacpp_timings_promotes_source_and_fills_durations() {
        let usage = serde_json::json!({
            "prompt_tokens": 220,
            "completion_tokens": 17,
            "timings": {
                "prompt_n": 200,
                "prompt_ms": 145.4,
                "predicted_n": 17,
                "predicted_ms": 89.1,
            }
        });

        let telemetry = ProviderTelemetry::from_openai_usage(&usage, None);

        assert_eq!(telemetry.source, source::LLAMACPP_TIMINGS);
        assert_eq!(telemetry.server_prompt_eval_ms, Some(145));
        assert_eq!(telemetry.server_generation_ms, Some(89));
        assert_eq!(telemetry.server_total_ms, Some(234));
        assert_eq!(telemetry.server_prompt_tokens, Some(200));
        assert_eq!(telemetry.server_output_tokens, Some(17));
        assert!(!telemetry.is_empty());
    }

    #[test]
    fn ps_entry_pulls_context_length_from_top_level_or_details() {
        let entry = serde_json::json!({
            "name": "qwen3.6:35b-a3b-coding-nvfp4",
            "size": 4_700_000_000u64,
            "size_vram": 4_500_000_000u64,
            "expires_at": "2026-05-14T10:30:00Z",
            "context_length": 32768
        });
        let model = OllamaPsModel::from_ps_entry(&entry).expect("ps entry parses");
        assert_eq!(model.context_length, Some(32768));

        let entry_nested = serde_json::json!({
            "name": "qwen3.6:35b-a3b-coding-nvfp4",
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
}
