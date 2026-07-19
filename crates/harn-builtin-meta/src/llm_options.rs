//! The canonical `llm_call` option registry — the single source of truth for
//! the public option surface shared by `llm_call`, `llm_call_safe`,
//! `llm_call_structured`, `llm_call_structured_result`, `llm_stream_call`,
//! and the agent loop's per-turn dispatch.
//!
//! Everything that names an option key derives from these tables:
//!
//! * the typechecker shape [`crate::shapes::LLM_CALL_OPTIONS`] wraps
//!   [`LLM_CALL_OPTION_FIELDS`] directly;
//! * the runtime extractor (`harn-vm`'s `extract_llm_options`) validates the
//!   caller-supplied dict against these keys and hard-errors on anything
//!   unknown (with a did-you-mean) or removed (with the recorded fix);
//! * the stdlib allowlist (`std/llm/options.llm_call_options`) is served by
//!   the `__llm_call_option_registry` builtin, which reads these tables;
//! * the `deprecated_llm_options` lint rule reports [`LLM_REMOVED_OPTIONS`]
//!   entries at `harn check` time.
//!
//! There is deliberately NO other list of accepted or removed option keys
//! anywhere in the workspace. One key, one spelling: synonyms are represented
//! only as [`LLM_REMOVED_OPTIONS`] entries carrying their replacement.
//!
//! Key-space rules:
//! * Keys starting with `_` are host/agent-loop plumbing channels (e.g.
//!   `_iteration`, `_system_fragments`, `_dispatch_provenance`). The runtime
//!   accepts them without validation and they are never documented or typed
//!   here.
//! * Provider-specific request shaping lives ONLY under
//!   `provider_options: {<provider>: {...}}` — provider names are not
//!   top-level keys.

use crate::{
    ShapeFieldDescriptor, Ty, TY_ANY, TY_BOOL, TY_DICT, TY_FLOAT, TY_INT, TY_LIST, TY_STRING,
};

const TY_BOOL_OR_DICT: Ty = Ty::Union(&[TY_BOOL, TY_DICT]);
const TY_BOOL_OR_STRING: Ty = Ty::Union(&[TY_BOOL, TY_STRING]);
const TY_BOOL_OR_STRING_OR_DICT: Ty = Ty::Union(&[TY_BOOL, TY_STRING, TY_DICT]);
const TY_LIST_OR_DICT: Ty = Ty::Union(&[TY_LIST, TY_DICT]);
const TY_NUM_OR_DICT: Ty = Ty::Union(&[TY_FLOAT, TY_INT, TY_DICT]);
const TY_STRING_OR_DICT: Ty = Ty::Union(&[TY_STRING, TY_DICT]);
const TY_STRING_OR_LIST: Ty = Ty::Union(&[TY_STRING, TY_LIST]);

/// The complete public option surface, in documentation order. This array is
/// the registry: the typechecker shape, the runtime unknown-key gate, the
/// stdlib allowlist, and the docs tables all enumerate exactly these keys.
pub const LLM_CALL_OPTION_FIELDS: &[ShapeFieldDescriptor] = &[
    // --- Routing ---
    ShapeFieldDescriptor::optional("model", TY_STRING),
    ShapeFieldDescriptor::optional("model_role", TY_STRING),
    ShapeFieldDescriptor::optional("model_tier", TY_STRING),
    ShapeFieldDescriptor::optional("provider", TY_STRING),
    ShapeFieldDescriptor::optional("api_mode", TY_STRING),
    ShapeFieldDescriptor::optional("route_policy", TY_STRING_OR_DICT),
    ShapeFieldDescriptor::optional("fallback_chain", TY_STRING_OR_LIST),
    ShapeFieldDescriptor::optional("routing", TY_DICT),
    ShapeFieldDescriptor::optional("equivalent_failover", TY_BOOL_OR_DICT),
    ShapeFieldDescriptor::optional("models", TY_LIST),
    ShapeFieldDescriptor::optional("ladder", TY_STRING),
    // --- Conversation ---
    ShapeFieldDescriptor::optional("system", TY_STRING_OR_LIST),
    ShapeFieldDescriptor::optional("messages", TY_LIST),
    ShapeFieldDescriptor::optional("session_id", TY_STRING),
    ShapeFieldDescriptor::optional("mock_scope", TY_STRING),
    ShapeFieldDescriptor::optional("context_profile", TY_DICT),
    ShapeFieldDescriptor::optional("capabilities", TY_ANY),
    ShapeFieldDescriptor::optional("prefill", TY_STRING),
    ShapeFieldDescriptor::optional("previous_response_id", TY_STRING),
    // --- Generation ---
    ShapeFieldDescriptor::optional("max_tokens", TY_INT),
    ShapeFieldDescriptor::optional("temperature", TY_FLOAT),
    ShapeFieldDescriptor::optional("top_p", TY_FLOAT),
    ShapeFieldDescriptor::optional("top_k", TY_INT),
    ShapeFieldDescriptor::optional("logprobs", TY_BOOL),
    ShapeFieldDescriptor::optional("top_logprobs", TY_INT),
    ShapeFieldDescriptor::optional("stop", TY_STRING_OR_LIST),
    ShapeFieldDescriptor::optional("stop_at_tool_call", TY_BOOL),
    ShapeFieldDescriptor::optional("seed", TY_INT),
    ShapeFieldDescriptor::optional("frequency_penalty", TY_FLOAT),
    ShapeFieldDescriptor::optional("presence_penalty", TY_FLOAT),
    // --- Output contract (one key; see OutputSpec in std/llm/options) ---
    ShapeFieldDescriptor::optional("output", TY_ANY),
    ShapeFieldDescriptor::optional("schema_retries", TY_INT),
    ShapeFieldDescriptor::optional("schema_retry_nudge", TY_BOOL_OR_STRING),
    ShapeFieldDescriptor::optional("retries", TY_INT),
    ShapeFieldDescriptor::optional("schema_recover", TY_BOOL),
    ShapeFieldDescriptor::optional("repair", TY_BOOL_OR_DICT),
    // --- Reasoning & modalities ---
    ShapeFieldDescriptor::optional("thinking", TY_BOOL_OR_STRING_OR_DICT),
    ShapeFieldDescriptor::optional("effort", TY_STRING),
    ShapeFieldDescriptor::optional("reasoning_policy", TY_ANY),
    ShapeFieldDescriptor::optional("reasoning_scale", TY_STRING),
    ShapeFieldDescriptor::optional("reasoning_task", TY_STRING),
    ShapeFieldDescriptor::optional("interleaved_thinking", TY_BOOL),
    ShapeFieldDescriptor::optional("anthropic_beta_features", TY_STRING_OR_LIST),
    ShapeFieldDescriptor::optional("vision", TY_BOOL),
    ShapeFieldDescriptor::optional("audio", TY_BOOL),
    ShapeFieldDescriptor::optional("pdf", TY_BOOL),
    ShapeFieldDescriptor::optional("video", TY_BOOL),
    // --- Tools ---
    ShapeFieldDescriptor::optional("tools", TY_LIST_OR_DICT),
    ShapeFieldDescriptor::optional("provider_tools", TY_LIST_OR_DICT),
    ShapeFieldDescriptor::optional("tool_choice", TY_STRING_OR_DICT),
    ShapeFieldDescriptor::optional("tool_search", TY_BOOL_OR_STRING_OR_DICT),
    ShapeFieldDescriptor::optional("tool_format", TY_STRING),
    // --- Caching, budgets, transport ---
    ShapeFieldDescriptor::optional("cache", TY_BOOL_OR_DICT),
    ShapeFieldDescriptor::optional("prompt_cache_ttl", TY_STRING),
    ShapeFieldDescriptor::optional("budget", TY_NUM_OR_DICT),
    ShapeFieldDescriptor::optional("timeout_ms", TY_INT),
    ShapeFieldDescriptor::optional("idle_timeout_ms", TY_INT),
    ShapeFieldDescriptor::optional("stream", TY_BOOL),
    ShapeFieldDescriptor::optional("speed", TY_STRING),
    // --- OpenAI Responses surface (require api_mode: "responses") ---
    ShapeFieldDescriptor::optional("store", TY_BOOL_OR_DICT),
    ShapeFieldDescriptor::optional("background", TY_BOOL),
    ShapeFieldDescriptor::optional("truncation", TY_STRING),
    ShapeFieldDescriptor::optional("compact", TY_BOOL),
    ShapeFieldDescriptor::optional("include", TY_LIST),
    ShapeFieldDescriptor::optional("max_tool_calls", TY_INT),
    // --- Provider escape hatch (namespaced; never silently dropped) ---
    ShapeFieldDescriptor::optional("provider_options", TY_DICT),
    // --- Observability & experiments ---
    ShapeFieldDescriptor::optional("metadata", TY_DICT),
    ShapeFieldDescriptor::optional("reminders", TY_ANY),
    ShapeFieldDescriptor::optional("structural_experiment", TY_ANY),
];

/// Wrapper-plane keys: accepted members of the public surface that the Rust
/// core deliberately does not read — they configure the stdlib caller stack
/// (`std/llm/safe`, `std/llm/refine`, `std/llm/handlers`) around the call.
/// Enumerated so the extractor can accept them knowingly (not as an
/// unknown-key escape) and docs can label them.
pub const LLM_WRAPPER_ONLY_KEYS: &[&str] = &[
    "schema_retries",
    "schema_retry_nudge",
    "retries",
    "schema_recover",
    "repair",
    "metadata",
];

/// A removed option key and the message telling the author exactly what to
/// write instead. Every synonym/alias the surface ever accepted lives here —
/// writing one is a hard error at runtime and a `harn check` lint error, never
/// a silent drop.
#[derive(Clone, Copy, Debug)]
pub struct RemovedLlmOption {
    pub key: &'static str,
    pub fix: &'static str,
}

const fn removed(key: &'static str, fix: &'static str) -> RemovedLlmOption {
    RemovedLlmOption { key, fix }
}

/// Removed keys with their replacements. Grouped by the surviving canonical
/// key. Provider names are listed individually so the error for
/// `{openai: {...}}` names the exact `provider_options` rewrite.
pub const LLM_REMOVED_OPTIONS: &[RemovedLlmOption] = &[
    // Output contract: one `output` key.
    removed(
        "schema",
        "use `output` (a schema value, or {schema, strict?, validation?, stream_abort?})",
    ),
    removed(
        "json_schema",
        "use `output` (a schema value, or {schema, ...})",
    ),
    removed(
        "output_schema",
        "use `output` (a schema value, or {schema, ...})",
    ),
    removed(
        "output_format",
        "use `output` (\"json\" | schema value | {schema, ...})",
    ),
    removed(
        "response_format",
        "use `output: \"json\"` or `output: <schema>`",
    ),
    removed(
        "output_validation",
        "use `output: {schema, validation: ...}`",
    ),
    removed(
        "schema_stream_abort",
        "use `output: {schema, stream_abort: ...}`",
    ),
    removed("llm_repair", "use `repair`"),
    // System prompt: one `system` key (string or ordered fragment list).
    removed(
        "system_preamble",
        "use `system: [{content, position: \"before\"}, ...]`",
    ),
    removed(
        "system_prefix",
        "use `system: [{content, position: \"before\"}, ...]`",
    ),
    removed(
        "system_context",
        "use `system: [{content, position: \"before\"}, ...]`",
    ),
    removed(
        "system_prompt_parts",
        "use `system: [...]` (the fragment list directly)",
    ),
    removed(
        "system_appendix",
        "use `system: [{content, position: \"after\"}, ...]`",
    ),
    removed(
        "system_suffix",
        "use `system: [{content, position: \"after\"}, ...]`",
    ),
    removed("caps", "use `capabilities`"),
    removed("project_context_profile", "use `context_profile`"),
    // Routing.
    removed("api", "use `api_mode`"),
    removed("role", "use `model_role`"),
    removed(
        "prefer",
        "use `route_policy: {mode: \"preference_list\", targets, strategy}`",
    ),
    removed(
        "fallback_strategy",
        "use `route_policy: {mode: \"preference_list\", targets, strategy}`",
    ),
    removed(
        "strategy",
        "use `route_policy: {mode: \"preference_list\", targets, strategy}`",
    ),
    removed(
        "model_ladder",
        "use `models` (inline steps) or `ladder` (named catalog ladder)",
    ),
    removed(
        "budget_usd",
        "use `budget: {max_cost_usd: ...}` (or a bare number for the same)",
    ),
    // Reasoning.
    removed("reasoning_effort", "use `effort`"),
    removed("thinking_policy", "use `reasoning_policy`"),
    removed("problem_scale", "use `reasoning_scale`"),
    removed("task_kind", "use `reasoning_task`"),
    removed("task", "use `reasoning_task`"),
    // Tools.
    removed("hosted_tools", "use `provider_tools`"),
    // Responses surface.
    removed("response_store", "use `store`"),
    removed("responses_store", "use `store`"),
    // Transport.
    removed("timeout", "use `timeout_ms` (milliseconds)"),
    removed("idle_timeout", "use `idle_timeout_ms` (milliseconds)"),
    removed("fast", "use `speed: \"fast\"`"),
    // Session lifecycle (removed pre-W2; kept here so the fix survives).
    removed(
        "transcript",
        "open or resume a session with agent_session_open(id) and pass `session_id: id`",
    ),
    // Internal channels that briefly leaked into the public dict.
    removed(
        "dispatch_provenance",
        "internal: resolvers set `_dispatch_provenance`",
    ),
    // Provider request shaping: namespaced under provider_options.
    removed("anthropic", "use `provider_options: {anthropic: {...}}`"),
    removed("openai", "use `provider_options: {openai: {...}}`"),
    removed("openrouter", "use `provider_options: {openrouter: {...}}`"),
    removed("together", "use `provider_options: {together: {...}}`"),
    removed("groq", "use `provider_options: {groq: {...}}`"),
    removed("cerebras", "use `provider_options: {cerebras: {...}}`"),
    removed("deepseek", "use `provider_options: {deepseek: {...}}`"),
    removed("fireworks", "use `provider_options: {fireworks: {...}}`"),
    removed(
        "huggingface",
        "use `provider_options: {huggingface: {...}}`",
    ),
    removed("local", "use `provider_options: {local: {...}}`"),
    removed("mlx", "use `provider_options: {mlx: {...}}`"),
    removed("vllm", "use `provider_options: {vllm: {...}}`"),
    removed("tgi", "use `provider_options: {tgi: {...}}`"),
    removed("dashscope", "use `provider_options: {dashscope: {...}}`"),
    removed("gemini", "use `provider_options: {gemini: {...}}`"),
    removed(
        "azure_openai",
        "use `provider_options: {azure_openai: {...}}`",
    ),
    removed("bedrock", "use `provider_options: {bedrock: {...}}`"),
    removed("ollama", "use `provider_options: {ollama: {...}}`"),
    removed("vertex", "use `provider_options: {vertex: {...}}`"),
    removed("mock", "use `provider_options: {mock: {...}}`"),
    removed("fake", "use `provider_options: {fake: {...}}`"),
];

/// True when `key` is an accepted public option.
pub fn is_llm_call_option(key: &str) -> bool {
    LLM_CALL_OPTION_FIELDS.iter().any(|field| field.name == key)
}

/// The removal record for `key`, if it names a removed option.
pub fn removed_llm_option(key: &str) -> Option<&'static RemovedLlmOption> {
    LLM_REMOVED_OPTIONS.iter().find(|entry| entry.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_keys_are_unique_and_disjoint_from_removed() {
        let mut seen = std::collections::BTreeSet::new();
        for field in LLM_CALL_OPTION_FIELDS {
            assert!(
                seen.insert(field.name),
                "duplicate registry key {}",
                field.name
            );
            assert!(
                removed_llm_option(field.name).is_none(),
                "{} is both accepted and removed",
                field.name
            );
            assert!(
                !field.name.starts_with('_'),
                "internal channel {} must not be in the public registry",
                field.name
            );
        }
        let mut removed_seen = std::collections::BTreeSet::new();
        for entry in LLM_REMOVED_OPTIONS {
            assert!(
                removed_seen.insert(entry.key),
                "duplicate removed key {}",
                entry.key
            );
            assert!(!entry.fix.is_empty());
        }
    }

    #[test]
    fn wrapper_only_keys_are_registered() {
        for key in LLM_WRAPPER_ONLY_KEYS {
            assert!(
                is_llm_call_option(key),
                "wrapper key {key} missing from registry"
            );
        }
    }
}
