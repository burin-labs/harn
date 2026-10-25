//! One registry for provider cache counter aliases.

const READ_FIELDS: &[&str] = &[
    "/cache_read_input_tokens",
    "/prompt_tokens_details/cached_tokens",
    "/input_tokens_details/cached_tokens",
    "/input_token_details/cached_tokens",
    "/cached_tokens",
    "/cache_read_tokens",
    "/cached_input_tokens",
    "/cached_prompt_tokens",
    "/prompt_cache_hit_tokens",
    "/cache/read_input_tokens",
    "/cacheReadInputTokens",
    "/cachedContentTokenCount",
    "/total_cached_tokens",
];
const WRITE_FIELDS: &[&str] = &[
    "/cache_write_tokens",
    "/cache_creation_input_tokens",
    "/prompt_tokens_details/cache_write_tokens",
    "/prompt_tokens_details/cache_creation_input_tokens",
    "/input_tokens_details/cache_write_tokens",
    "/input_tokens_details/cache_creation_input_tokens",
    "/cache/write_input_tokens",
    "/cacheWriteInputTokens",
];

/// Top-level members the alias registry reads. A caller holding a large value,
/// such as a whole LLM result, can convert only these members to JSON.
pub fn cache_field_roots() -> impl Iterator<Item = &'static str> {
    READ_FIELDS.iter().chain(WRITE_FIELDS).filter_map(|path| {
        path.strip_prefix('/')
            .and_then(|path| path.split('/').next())
    })
}

pub fn reported_cache_read_tokens(value: &serde_json::Value) -> Result<Option<i64>, &'static str> {
    reported_counter(value, READ_FIELDS)
}

pub fn reported_cache_write_tokens(value: &serde_json::Value) -> Result<Option<i64>, &'static str> {
    reported_counter(value, WRITE_FIELDS)
}

/// Cache counters parsed from one usage object. The counts default to zero for
/// arithmetic, but `reported` is true only when the response carried at least
/// one cache counter, so an omitted field never reads as a measured zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheCounts {
    pub read_tokens: i64,
    pub write_tokens: i64,
    pub reported: bool,
}

pub fn extract_cache_counts(
    value: &serde_json::Value,
) -> Result<CacheCounts, harn_vm_core::VmError> {
    let read = reported_cache_read_tokens(value).map_err(invalid_usage)?;
    let write = reported_cache_write_tokens(value).map_err(invalid_usage)?;
    Ok(CacheCounts {
        read_tokens: read.unwrap_or(0),
        write_tokens: write.unwrap_or(0),
        reported: read.is_some() || write.is_some(),
    })
}

fn invalid_usage(reason: &'static str) -> harn_vm_core::VmError {
    harn_vm_core::VmError::Runtime(format!("invalid provider cache usage: {reason}"))
}

fn reported_counter(
    value: &serde_json::Value,
    paths: &[&str],
) -> Result<Option<i64>, &'static str> {
    let mut reported = None;
    for path in paths {
        if let Some(value) = value.pointer(path) {
            if value.is_null() {
                continue;
            }
            let count = value.as_i64().ok_or("token counter is not an integer")?;
            if count < 0 {
                return Err("negative token count");
            }
            if reported.is_some_and(|previous| previous != count) {
                return Err("token counter aliases disagree");
            }
            reported = Some(count);
        }
    }
    Ok(reported)
}
