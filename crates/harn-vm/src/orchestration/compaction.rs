//! Auto-compaction — transcript size management strategies.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::llm::{vm_call_llm_full, vm_value_to_json};
use crate::value::{VmError, VmValue};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompactStrategy {
    Llm,
    Truncate,
    Custom,
    ObservationMask,
}

pub fn parse_compact_strategy(value: &str) -> Result<CompactStrategy, VmError> {
    match value {
        "llm" => Ok(CompactStrategy::Llm),
        "truncate" => Ok(CompactStrategy::Truncate),
        "custom" => Ok(CompactStrategy::Custom),
        "observation_mask" => Ok(CompactStrategy::ObservationMask),
        other => Err(VmError::Runtime(format!(
            "unknown compact_strategy '{other}' (expected 'llm', 'truncate', 'custom', or 'observation_mask')"
        ))),
    }
}

pub fn compact_strategy_name(strategy: &CompactStrategy) -> &'static str {
    match strategy {
        CompactStrategy::Llm => "llm",
        CompactStrategy::Truncate => "truncate",
        CompactStrategy::Custom => "custom",
        CompactStrategy::ObservationMask => "observation_mask",
    }
}

/// Configuration for automatic transcript compaction in agent loops.
///
/// Two-tier compaction:
///   Tier 1 (`token_threshold` / `compact_strategy`): lightweight, deterministic
///     observation masking that fires early. Masks verbose tool results while
///     preserving assistant prose and error output.
///   Tier 2 (`hard_limit_tokens` / `hard_limit_strategy`): aggressive LLM-powered
///     summarization that fires when tier-1 alone isn't enough, typically as the
///     transcript approaches the model's actual context window.
#[derive(Clone, Debug)]
pub struct AutoCompactConfig {
    /// Tier-1 threshold: estimated tokens before lightweight compaction.
    pub token_threshold: usize,
    /// Maximum character length for a single tool result before microcompaction.
    pub tool_output_max_chars: usize,
    /// Number of recent messages to keep during compaction.
    pub keep_last: usize,
    /// Tier-1 strategy (default: ObservationMask).
    pub compact_strategy: CompactStrategy,
    /// Tier-2 threshold: fires when tier-1 result still exceeds this.
    /// Typically set to ~75% of the model's actual context window.
    /// When `None`, tier-2 is disabled.
    pub hard_limit_tokens: Option<usize>,
    /// Tier-2 strategy (default: Llm).
    pub hard_limit_strategy: CompactStrategy,
    /// Optional Harn callback used when a strategy is `custom`.
    pub custom_compactor: Option<VmValue>,
    /// Optional callback for domain-specific per-message masking during
    /// observation mask compaction. Called with a list of archived messages,
    /// returns a list of `Option<String>` — `Some(masked)` to override the
    /// default mask for that message, `None` to use the default.
    /// This lets the host (e.g. burin-code) inject AST outlines, file
    /// summaries, etc. without putting language-specific logic in Harn.
    pub mask_callback: Option<VmValue>,
    /// Optional callback for per-tool-result compression. Called with
    /// `{tool_name, output, max_chars}` and returns compressed output string.
    /// When set, used INSTEAD of the built-in `microcompact_tool_output`.
    /// This allows the pipeline to use LLM-based compression rather than
    /// keyword heuristics.
    pub compress_callback: Option<VmValue>,
    /// Optional prompt-template asset path used when LLM compaction is
    /// selected. The rendered template becomes the user message sent to
    /// the summarizer.
    pub summarize_prompt: Option<String>,
}

impl Default for AutoCompactConfig {
    fn default() -> Self {
        Self {
            token_threshold: 48_000,
            tool_output_max_chars: 16_000,
            keep_last: 12,
            compact_strategy: CompactStrategy::ObservationMask,
            hard_limit_tokens: None,
            hard_limit_strategy: CompactStrategy::Llm,
            custom_compactor: None,
            mask_callback: None,
            compress_callback: None,
            summarize_prompt: None,
        }
    }
}

/// Estimate token count from a list of JSON messages (chars / 4 heuristic).
pub fn estimate_message_tokens(messages: &[serde_json::Value]) -> usize {
    messages
        .iter()
        .map(|m| {
            m.get("content")
                .and_then(|c| c.as_str())
                .map(|s| s.len())
                .unwrap_or(0)
        })
        .sum::<usize>()
        / 4
}

fn is_reasoning_or_tool_turn_message(message: &serde_json::Value) -> bool {
    let role = message
        .get("role")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    role == "tool"
        || message.get("tool_calls").is_some()
        || message
            .get("reasoning")
            .map(|value| !value.is_null())
            .unwrap_or(false)
}

fn find_prev_user_boundary(messages: &[serde_json::Value], start: usize) -> Option<usize> {
    (0..=start)
        .rev()
        .find(|idx| messages[*idx].get("role").and_then(|value| value.as_str()) == Some("user"))
}

/// Microcompact a tool result: if it exceeds `max_chars`, keep the first and
/// last portions with a snip marker in between.
pub fn microcompact_tool_output(output: &str, max_chars: usize) -> String {
    if output.len() <= max_chars || max_chars < 200 {
        return output.to_string();
    }
    let diagnostic_lines = output
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            let lower = trimmed.to_lowercase();
            let has_file_line = {
                let bytes = trimmed.as_bytes();
                let mut i = 0;
                let mut found_colon = false;
                while i < bytes.len() {
                    if bytes[i] == b':' {
                        found_colon = true;
                        break;
                    }
                    i += 1;
                }
                found_colon && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit()
            };
            let has_strong_keyword =
                trimmed.contains("FAIL") || trimmed.contains("panic") || trimmed.contains("Panic");
            let has_weak_keyword = trimmed.contains("error")
                || trimmed.contains("undefined")
                || trimmed.contains("expected")
                || trimmed.contains("got")
                || lower.contains("cannot find")
                || lower.contains("not found")
                || lower.contains("no such")
                || lower.contains("unresolved")
                || lower.contains("missing")
                || lower.contains("declared but not used")
                || lower.contains("unused")
                || lower.contains("mismatch");
            let positional = lower.contains(" error ")
                || lower.starts_with("error:")
                || lower.starts_with("warning:")
                || lower.starts_with("note:")
                || lower.contains("panic:");
            has_strong_keyword || (has_file_line && has_weak_keyword) || positional
        })
        .take(32)
        .collect::<Vec<_>>();
    if !diagnostic_lines.is_empty() {
        let diagnostics = diagnostic_lines.join("\n");
        let budget = max_chars.saturating_sub(diagnostics.len() + 64);
        let keep = budget / 2;
        if keep >= 80 && output.len() > keep * 2 {
            let head = snap_to_line_end(output, keep);
            let tail = snap_to_line_start(output, output.len().saturating_sub(keep));
            return format!(
                "{head}\n\n[diagnostic lines preserved]\n{diagnostics}\n\n[... output compacted ...]\n\n{tail}"
            );
        }
    }
    let keep = max_chars / 2;
    let head = snap_to_line_end(output, keep);
    let tail = snap_to_line_start(output, output.len().saturating_sub(keep));
    let snipped = output.len().saturating_sub(head.len() + tail.len());
    format!("{head}\n\n[... {snipped} characters snipped ...]\n\n{tail}")
}

/// Invoke the compress_callback to compress a tool result via pipeline-defined
/// logic (typically an LLM call). Returns the compressed output, or falls back
/// to `microcompact_tool_output` on error.
pub(crate) async fn invoke_compress_callback(
    callback: &VmValue,
    tool_name: &str,
    output: &str,
    max_chars: usize,
) -> String {
    let VmValue::Closure(closure) = callback.clone() else {
        return microcompact_tool_output(output, max_chars);
    };
    let mut vm = match crate::vm::clone_async_builtin_child_vm() {
        Some(vm) => vm,
        None => return microcompact_tool_output(output, max_chars),
    };
    let args_dict = VmValue::Dict(Rc::new({
        let mut dict = std::collections::BTreeMap::new();
        dict.insert(
            "tool_name".to_string(),
            VmValue::String(Rc::from(tool_name)),
        );
        dict.insert("output".to_string(), VmValue::String(Rc::from(output)));
        dict.insert("max_chars".to_string(), VmValue::Int(max_chars as i64));
        dict
    }));
    match vm.call_closure_pub(&closure, &[args_dict]).await {
        Ok(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => microcompact_tool_output(output, max_chars),
    }
}

/// Snap a byte offset to the nearest preceding line boundary (end of a complete line).
/// Returns the substring from the start up to and including the last complete line
/// that fits within `max_bytes`. Never cuts mid-line.
fn snap_to_line_end(s: &str, max_bytes: usize) -> &str {
    if max_bytes >= s.len() {
        return s;
    }
    let search_end = s.floor_char_boundary(max_bytes);
    match s[..search_end].rfind('\n') {
        Some(pos) => &s[..pos + 1],
        None => &s[..search_end], // single long line — fall back to char boundary
    }
}

/// Snap a byte offset to the nearest following line boundary (start of a complete line).
/// Returns the substring from the first complete line at or after `start_byte`.
/// Never cuts mid-line.
fn snap_to_line_start(s: &str, start_byte: usize) -> &str {
    if start_byte == 0 {
        return s;
    }
    let search_start = s.ceil_char_boundary(start_byte);
    if search_start >= s.len() {
        return "";
    }
    match s[search_start..].find('\n') {
        Some(pos) => {
            let line_start = search_start + pos + 1;
            if line_start < s.len() {
                &s[line_start..]
            } else {
                &s[search_start..]
            }
        }
        None => &s[search_start..], // already at start of last line
    }
}

fn format_compaction_messages(messages: &[serde_json::Value]) -> String {
    messages
        .iter()
        .map(|msg| {
            let role = msg
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("user")
                .to_uppercase();
            let content = msg
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            format!("{role}: {content}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate_compaction_summary(
    old_messages: &[serde_json::Value],
    archived_count: usize,
) -> String {
    truncate_compaction_summary_with_context(old_messages, archived_count, false)
}

fn truncate_compaction_summary_with_context(
    old_messages: &[serde_json::Value],
    archived_count: usize,
    is_llm_fallback: bool,
) -> String {
    let per_msg_limit = 500_usize;
    let summary_parts: Vec<String> = old_messages
        .iter()
        .filter_map(|m| {
            let role = m.get("role")?.as_str()?;
            let content = m.get("content")?.as_str()?;
            if content.is_empty() {
                return None;
            }
            let truncated = if content.len() > per_msg_limit {
                format!(
                    "{}... [truncated from {} chars]",
                    &content[..content.floor_char_boundary(per_msg_limit)],
                    content.len()
                )
            } else {
                content.to_string()
            };
            Some(format!("[{role}] {truncated}"))
        })
        .take(15)
        .collect();
    let header = if is_llm_fallback {
        format!(
            "[auto-compact fallback: LLM summarizer returned empty; {archived_count} older messages abbreviated to ~{per_msg_limit} chars each]"
        )
    } else {
        format!("[auto-compacted {archived_count} older messages via truncate strategy]")
    };
    format!(
        "{header}\n{}{}",
        summary_parts.join("\n"),
        if archived_count > 15 {
            format!("\n... and {} more", archived_count - 15)
        } else {
            String::new()
        }
    )
}

fn compact_summary_text_from_value(value: &VmValue) -> Result<String, VmError> {
    if let Some(map) = value.as_dict() {
        if let Some(summary) = map.get("summary").or_else(|| map.get("text")) {
            return Ok(summary.display());
        }
    }
    match value {
        VmValue::String(text) => Ok(text.to_string()),
        VmValue::Nil => Ok(String::new()),
        _ => serde_json::to_string_pretty(&vm_value_to_json(value))
            .map_err(|e| VmError::Runtime(format!("custom compactor encode error: {e}"))),
    }
}

async fn llm_compaction_summary(
    old_messages: &[serde_json::Value],
    archived_count: usize,
    llm_opts: &crate::llm::api::LlmCallOptions,
    summarize_prompt: Option<&str>,
) -> Result<String, VmError> {
    let mut compact_opts = llm_opts.clone();
    let formatted = format_compaction_messages(old_messages);
    compact_opts.system = None;
    compact_opts.transcript_summary = None;
    compact_opts.native_tools = None;
    compact_opts.tool_choice = None;
    compact_opts.response_format = None;
    compact_opts.json_schema = None;
    let prompt = render_llm_compaction_prompt(summarize_prompt, &formatted, archived_count)?;
    compact_opts.messages = vec![serde_json::json!({
        "role": "user",
        "content": prompt,
    })];
    let result = vm_call_llm_full(&compact_opts).await?;
    let summary = result.text.trim();
    if summary.is_empty() {
        Ok(truncate_compaction_summary_with_context(
            old_messages,
            archived_count,
            true,
        ))
    } else {
        Ok(format!(
            "[auto-compacted {archived_count} older messages]\n{summary}"
        ))
    }
}

fn render_llm_compaction_prompt(
    summarize_prompt: Option<&str>,
    formatted: &str,
    archived_count: usize,
) -> Result<String, VmError> {
    let Some(path) = summarize_prompt.filter(|path| !path.trim().is_empty()) else {
        return Ok(format!(
            "Summarize these archived conversation messages for a follow-on coding agent. Preserve goals, constraints, decisions, completed tool work, unresolved issues, and next actions. Output only the summary text.\n\nArchived message count: {archived_count}\n\nConversation:\n{formatted}"
        ));
    };

    let resolved = crate::stdlib::process::resolve_source_asset_path(path);
    let template = std::fs::read_to_string(&resolved).map_err(|error| {
        VmError::Runtime(format!(
            "failed to read compaction summarize_prompt {}: {error}",
            resolved.display()
        ))
    })?;
    let mut bindings = BTreeMap::new();
    bindings.insert(
        "formatted_messages".to_string(),
        VmValue::String(Rc::from(formatted.to_string())),
    );
    bindings.insert(
        "archived_count".to_string(),
        VmValue::Int(archived_count as i64),
    );
    crate::stdlib::template::render_template_result(
        &template,
        Some(&bindings),
        resolved.parent(),
        Some(&resolved),
    )
    .map_err(|error| {
        VmError::Runtime(format!(
            "compaction summarize_prompt render error: {error:?}"
        ))
    })
}

async fn custom_compaction_summary(
    old_messages: &[serde_json::Value],
    archived_count: usize,
    callback: &VmValue,
) -> Result<String, VmError> {
    let Some(VmValue::Closure(closure)) = Some(callback.clone()) else {
        return Err(VmError::Runtime(
            "compact_callback must be a closure when compact_strategy is 'custom'".to_string(),
        ));
    };
    let mut vm = crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
        VmError::Runtime(
            "custom transcript compaction requires an async builtin VM context".to_string(),
        )
    })?;
    let messages_vm = VmValue::List(Rc::new(
        old_messages
            .iter()
            .map(crate::stdlib::json_to_vm_value)
            .collect(),
    ));
    let result = vm.call_closure_pub(&closure, &[messages_vm]).await;
    let summary = compact_summary_text_from_value(&result?)?;
    if summary.trim().is_empty() {
        Ok(truncate_compaction_summary(old_messages, archived_count))
    } else {
        Ok(format!(
            "[auto-compacted {archived_count} older messages]\n{summary}"
        ))
    }
}

/// Check whether a tool-result string should be preserved verbatim during
/// observation masking. Uses content length as the primary heuristic:
/// short results (< 500 chars) are kept since they're typically error messages,
/// status lines, or concise answers that are cheap to retain and risky to mask.
/// Long results are masked to save context budget.
fn content_should_preserve(content: &str) -> bool {
    content.len() < 500
}

/// Default per-message masking for tool results.
fn default_mask_tool_result(role: &str, content: &str) -> String {
    let first_line = content.lines().next().unwrap_or(content);
    let line_count = content.lines().count();
    let char_count = content.len();
    if line_count <= 3 {
        format!("[{role}] {content}")
    } else {
        let preview = &first_line[..first_line.len().min(120)];
        format!("[{role}] {preview}... [{line_count} lines, {char_count} chars masked]")
    }
}

/// Deterministic observation-mask compaction.
#[cfg(test)]
pub(crate) fn observation_mask_compaction(
    old_messages: &[serde_json::Value],
    archived_count: usize,
) -> String {
    observation_mask_compaction_with_callback(old_messages, archived_count, None)
}

fn observation_mask_compaction_with_callback(
    old_messages: &[serde_json::Value],
    archived_count: usize,
    mask_results: Option<&[Option<String>]>,
) -> String {
    let mut parts = Vec::new();
    parts.push(format!(
        "[auto-compacted {archived_count} older messages via observation masking]"
    ));
    for (idx, msg) in old_messages.iter().enumerate() {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let content = msg
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if content.is_empty() {
            continue;
        }
        if role == "assistant" {
            parts.push(format!("[assistant] {content}"));
            continue;
        }
        if content_should_preserve(content) {
            parts.push(format!("[{role}] {content}"));
        } else if let Some(Some(custom)) = mask_results.and_then(|r| r.get(idx)) {
            parts.push(custom.clone());
        } else {
            parts.push(default_mask_tool_result(role, content));
        }
    }
    parts.join("\n")
}

/// Invoke the mask_callback to get per-message custom masks.
async fn invoke_mask_callback(
    callback: &VmValue,
    old_messages: &[serde_json::Value],
) -> Result<Vec<Option<String>>, VmError> {
    let VmValue::Closure(closure) = callback.clone() else {
        return Err(VmError::Runtime(
            "mask_callback must be a closure".to_string(),
        ));
    };
    let mut vm = crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
        VmError::Runtime("mask_callback requires an async builtin VM context".to_string())
    })?;
    let messages_vm = VmValue::List(Rc::new(
        old_messages
            .iter()
            .map(crate::stdlib::json_to_vm_value)
            .collect(),
    ));
    let result = vm.call_closure_pub(&closure, &[messages_vm]).await?;
    let list = match result {
        VmValue::List(items) => items,
        _ => return Ok(vec![None; old_messages.len()]),
    };
    Ok(list
        .iter()
        .map(|v| match v {
            VmValue::String(s) => Some(s.to_string()),
            VmValue::Nil => None,
            _ => None,
        })
        .collect())
}

/// Apply a single compaction strategy to a list of archived messages.
async fn apply_compaction_strategy(
    strategy: &CompactStrategy,
    old_messages: &[serde_json::Value],
    archived_count: usize,
    llm_opts: Option<&crate::llm::api::LlmCallOptions>,
    custom_compactor: Option<&VmValue>,
    mask_callback: Option<&VmValue>,
    summarize_prompt: Option<&str>,
) -> Result<String, VmError> {
    match strategy {
        CompactStrategy::Truncate => Ok(truncate_compaction_summary(old_messages, archived_count)),
        CompactStrategy::Llm => {
            llm_compaction_summary(
                old_messages,
                archived_count,
                llm_opts.ok_or_else(|| {
                    VmError::Runtime(
                        "LLM transcript compaction requires active LLM call options".to_string(),
                    )
                })?,
                summarize_prompt,
            )
            .await
        }
        CompactStrategy::Custom => {
            custom_compaction_summary(
                old_messages,
                archived_count,
                custom_compactor.ok_or_else(|| {
                    VmError::Runtime(
                        "compact_callback is required when compact_strategy is 'custom'"
                            .to_string(),
                    )
                })?,
            )
            .await
        }
        CompactStrategy::ObservationMask => {
            let mask_results = if let Some(cb) = mask_callback {
                Some(invoke_mask_callback(cb, old_messages).await?)
            } else {
                None
            };
            Ok(observation_mask_compaction_with_callback(
                old_messages,
                archived_count,
                mask_results.as_deref(),
            ))
        }
    }
}

/// Auto-compact a message list in place using two-tier compaction.
pub(crate) async fn auto_compact_messages(
    messages: &mut Vec<serde_json::Value>,
    config: &AutoCompactConfig,
    llm_opts: Option<&crate::llm::api::LlmCallOptions>,
) -> Result<Option<String>, VmError> {
    if messages.len() <= config.keep_last {
        return Ok(None);
    }
    let original_split = messages.len().saturating_sub(config.keep_last);
    let mut split_at = original_split;
    // Snap back to a user-role boundary so the kept suffix begins at a clean
    // turn. OpenAI-compatible APIs reject tool results orphaned from their
    // assistant request, so splitting mid-turn corrupts the transcript.
    while split_at > 0
        && messages[split_at]
            .get("role")
            .and_then(|r| r.as_str())
            .is_none_or(|r| r != "user")
    {
        split_at -= 1;
    }
    // Fall back to the naive split (e.g. tool-heavy transcripts with the sole
    // user message at index 0) rather than skipping compaction entirely.
    if split_at == 0 {
        split_at = original_split;
    }
    if let Some(volatile_start) = messages[split_at..]
        .iter()
        .position(is_reasoning_or_tool_turn_message)
        .map(|offset| split_at + offset)
    {
        if let Some(boundary) = volatile_start
            .checked_sub(1)
            .and_then(|idx| find_prev_user_boundary(messages, idx))
            .filter(|boundary| *boundary > 0)
        {
            split_at = boundary;
        }
    }
    if split_at == 0 {
        return Ok(None);
    }
    let old_messages: Vec<_> = messages.drain(..split_at).collect();
    let archived_count = old_messages.len();

    let mut summary = apply_compaction_strategy(
        &config.compact_strategy,
        &old_messages,
        archived_count,
        llm_opts,
        config.custom_compactor.as_ref(),
        config.mask_callback.as_ref(),
        config.summarize_prompt.as_deref(),
    )
    .await?;

    if let Some(hard_limit) = config.hard_limit_tokens {
        let summary_msg = serde_json::json!({"role": "user", "content": &summary});
        let mut estimate_msgs = vec![summary_msg];
        estimate_msgs.extend_from_slice(messages.as_slice());
        let estimated = estimate_message_tokens(&estimate_msgs);
        if estimated > hard_limit {
            let tier1_as_messages = vec![serde_json::json!({
                "role": "user",
                "content": summary,
            })];
            summary = apply_compaction_strategy(
                &config.hard_limit_strategy,
                &tier1_as_messages,
                archived_count,
                llm_opts,
                config.custom_compactor.as_ref(),
                None,
                config.summarize_prompt.as_deref(),
            )
            .await?;
        }
    }

    messages.insert(
        0,
        serde_json::json!({
            "role": "user",
            "content": summary,
        }),
    );
    Ok(Some(summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microcompact_short_output_unchanged() {
        let output = "line1\nline2\nline3\n";
        assert_eq!(microcompact_tool_output(output, 1000), output);
    }

    #[test]
    fn microcompact_snaps_to_line_boundaries() {
        let lines: Vec<String> = (0..20)
            .map(|i| format!("line {:02} content here", i))
            .collect();
        let output = lines.join("\n");
        let result = microcompact_tool_output(&output, 200);
        assert!(result.contains("[... "), "should have snip marker");
        let parts: Vec<&str> = result.split("\n\n[... ").collect();
        assert!(parts.len() >= 2, "should split at marker");
        let head = parts[0];
        for line in head.lines() {
            assert!(
                line.starts_with("line "),
                "head line should be complete: {line}"
            );
        }
    }

    #[test]
    fn microcompact_preserves_diagnostic_lines_with_line_boundaries() {
        let mut lines = Vec::new();
        for i in 0..50 {
            lines.push(format!("verbose output line {i}"));
        }
        lines.push("src/main.rs:42: error: cannot find value".to_string());
        for i in 50..100 {
            lines.push(format!("verbose output line {i}"));
        }
        let output = lines.join("\n");
        let result = microcompact_tool_output(&output, 600);
        assert!(result.contains("cannot find value"), "diagnostic preserved");
        assert!(
            result.contains("[diagnostic lines preserved]"),
            "has diagnostic marker"
        );
    }

    #[test]
    fn snap_to_line_end_finds_newline() {
        let s = "line1\nline2\nline3\nline4\n";
        let head = snap_to_line_end(s, 12);
        assert!(head.ends_with('\n'), "should end at newline");
        assert!(head.contains("line1"));
    }

    #[test]
    fn snap_to_line_start_finds_newline() {
        let s = "line1\nline2\nline3\nline4\n";
        let tail = snap_to_line_start(s, 12);
        assert!(
            tail.starts_with("line"),
            "should start at line boundary: {tail}"
        );
    }

    #[test]
    fn auto_compact_preserves_reasoning_tool_suffix() {
        let mut messages = vec![
            serde_json::json!({"role": "user", "content": "old task"}),
            serde_json::json!({"role": "assistant", "content": "old reply"}),
            serde_json::json!({"role": "user", "content": "new task"}),
            serde_json::json!({
                "role": "assistant",
                "content": "",
                "reasoning": "think first",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "read", "arguments": "{\"path\":\"foo.rs\"}"}
                }],
            }),
            serde_json::json!({"role": "tool", "tool_call_id": "call_1", "content": "file"}),
        ];
        let config = AutoCompactConfig {
            keep_last: 2,
            ..Default::default()
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let summary = runtime
            .block_on(auto_compact_messages(&mut messages, &config, None))
            .expect("compaction succeeds");

        assert!(summary.is_some());
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[2]["tool_calls"][0]["id"], "call_1");
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["tool_call_id"], "call_1");
    }
}
