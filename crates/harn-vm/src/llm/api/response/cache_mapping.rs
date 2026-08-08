//! Provider prompt-cache usage mapping.
//!
//! Providers report cached prompt tokens under three different field shapes.
//! This is the one place that knows all of them, so a parser asks for a count
//! rather than carrying a per-provider table of its own.

/// Extract cache-read token count from a provider `usage` JSON value,
/// covering Anthropic, OpenAI (and OpenAI-compatibles), and OpenRouter
/// passthrough field shapes. Returns 0 when the provider doesn't report it.
pub(crate) fn extract_cache_read_tokens(usage: &serde_json::Value) -> i64 {
    // Anthropic / OpenRouter passthrough: usage.cache_read_input_tokens
    if let Some(n) = usage
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_i64())
    {
        return n;
    }
    // OpenAI (and vLLM/SGLang when configured): usage.prompt_tokens_details.cached_tokens
    if let Some(n) = usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_i64())
    {
        return n;
    }
    if let Some(n) = usage
        .get("input_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_i64())
    {
        return n;
    }
    // OpenRouter variants: cache_read_tokens / cached_prompt_tokens.
    if let Some(n) = usage.get("cache_read_tokens").and_then(|v| v.as_i64()) {
        return n;
    }
    if let Some(n) = usage.get("cached_input_tokens").and_then(|v| v.as_i64()) {
        return n;
    }
    if let Some(n) = usage.get("cached_prompt_tokens").and_then(|v| v.as_i64()) {
        return n;
    }
    // DeepSeek (and a few OpenRouter passthrough shapes):
    // usage.prompt_cache_hit_tokens. Falling through to 0 silently hides
    // genuine cache hits when this is the only field the provider sets.
    if let Some(n) = usage
        .get("prompt_cache_hit_tokens")
        .and_then(|v| v.as_i64())
    {
        return n;
    }
    // OpenRouter `cache_discount` shape: `usage.cache.read_input_tokens`
    // (newer 2026-04 wire format their docs reference under "Caching →
    // Anthropic / Claude").
    if let Some(n) = usage
        .get("cache")
        .and_then(|d| d.get("read_input_tokens"))
        .and_then(|v| v.as_i64())
    {
        return n;
    }
    0
}

/// Extract cache-write (creation) token count from a provider `usage` JSON.
/// Anthropic reports this at top level; OpenRouter/OpenAI-compatible
/// providers may nest it under `prompt_tokens_details`.
pub(crate) fn extract_cache_write_tokens(usage: &serde_json::Value) -> i64 {
    if let Some(n) = usage.get("cache_write_tokens").and_then(|v| v.as_i64()) {
        return n;
    }
    if let Some(n) = usage
        .get("cache_creation_input_tokens")
        .and_then(|v| v.as_i64())
    {
        return n;
    }
    if let Some(n) = usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cache_write_tokens"))
        .and_then(|v| v.as_i64())
    {
        return n;
    }
    if let Some(n) = usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cache_creation_input_tokens"))
        .and_then(|v| v.as_i64())
    {
        return n;
    }
    if let Some(n) = usage
        .get("input_tokens_details")
        .and_then(|d| d.get("cache_write_tokens"))
        .and_then(|v| v.as_i64())
    {
        return n;
    }
    if let Some(n) = usage
        .get("input_tokens_details")
        .and_then(|d| d.get("cache_creation_input_tokens"))
        .and_then(|v| v.as_i64())
    {
        return n;
    }
    // OpenRouter newer `cache.write_input_tokens` shape — matches the
    // counterpart added to `extract_cache_read_tokens` above.
    if let Some(n) = usage
        .get("cache")
        .and_then(|d| d.get("write_input_tokens"))
        .and_then(|v| v.as_i64())
    {
        return n;
    }
    0
}
