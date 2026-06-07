//! Auto-compaction — transcript size management strategies.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::llm::{vm_call_llm_full, vm_value_to_json};
use crate::value::{VmError, VmValue};
use crate::vm::AsyncBuiltinCtx;

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

const COMPACTION_POLICY_KEYS: &[&str] = &[
    "instructions",
    "mode",
    "scope",
    "preserve",
    "drop",
    "extend_default_instructions",
    "author",
];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionPolicy {
    pub instructions: Option<String>,
    pub mode: Option<String>,
    pub scope: Option<String>,
    pub preserve: Vec<String>,
    #[serde(rename = "drop")]
    pub drop_items: Vec<String>,
    pub extend_default_instructions: Option<bool>,
    pub author: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionRequest {
    pub mode: Option<String>,
    pub policy: CompactionPolicy,
}

impl CompactionPolicy {
    pub fn has_metadata(&self) -> bool {
        self.instructions.is_some()
            || self.mode.is_some()
            || self.scope.is_some()
            || !self.preserve.is_empty()
            || !self.drop_items.is_empty()
            || self.extend_default_instructions.is_some()
            || self.author.is_some()
    }

    fn has_prompt_directives(&self) -> bool {
        self.instructions
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            || !self.preserve.is_empty()
            || !self.drop_items.is_empty()
    }

    pub fn instruction_mode(&self) -> &'static str {
        if !self.has_prompt_directives() {
            "default"
        } else if self.extend_default_instructions == Some(false) {
            "replace"
        } else {
            "extend"
        }
    }

    pub fn instruction_source(&self) -> Option<&str> {
        self.author
            .as_deref()
            .filter(|author| !author.trim().is_empty())
    }

    pub fn metadata_json(&self) -> Option<serde_json::Value> {
        if !self.has_metadata() {
            return None;
        }
        let mut map = serde_json::Map::new();
        if let Some(instructions) = self.instructions.as_ref() {
            map.insert(
                "instructions".to_string(),
                serde_json::Value::String(instructions.clone()),
            );
        }
        if let Some(mode) = self.mode.as_ref() {
            map.insert("mode".to_string(), serde_json::Value::String(mode.clone()));
        }
        if let Some(scope) = self.scope.as_ref() {
            map.insert(
                "scope".to_string(),
                serde_json::Value::String(scope.clone()),
            );
        }
        if !self.preserve.is_empty() {
            map.insert(
                "preserve".to_string(),
                serde_json::to_value(&self.preserve).unwrap_or_default(),
            );
        }
        if !self.drop_items.is_empty() {
            map.insert(
                "drop".to_string(),
                serde_json::to_value(&self.drop_items).unwrap_or_default(),
            );
        }
        if let Some(extend_default_instructions) = self.extend_default_instructions {
            map.insert(
                "extend_default_instructions".to_string(),
                serde_json::Value::Bool(extend_default_instructions),
            );
        }
        if let Some(author) = self.author.as_ref() {
            map.insert(
                "author".to_string(),
                serde_json::Value::String(author.clone()),
            );
        }
        map.insert(
            "instruction_mode".to_string(),
            serde_json::Value::String(self.instruction_mode().to_string()),
        );
        if let Some(source) = self.instruction_source() {
            map.insert(
                "instruction_source".to_string(),
                serde_json::Value::String(source.to_string()),
            );
        }
        Some(serde_json::Value::Object(map))
    }

    fn prompt_directives(&self) -> Option<String> {
        if !self.has_prompt_directives() {
            return None;
        }
        let mut parts = Vec::new();
        if let Some(instructions) = self
            .instructions
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            parts.push(instructions.to_string());
        }
        if !self.preserve.is_empty() {
            parts.push(format!("Preserve: {}.", self.preserve.join("; ")));
        }
        if !self.drop_items.is_empty() {
            parts.push(format!("Drop: {}.", self.drop_items.join("; ")));
        }
        Some(parts.join("\n"))
    }

    fn is_model_visible_scope(&self) -> bool {
        matches!(
            self.scope.as_deref(),
            Some("model_visible" | "summary" | "transcript")
        )
    }
}

pub fn compaction_policy_option_keys() -> &'static [&'static str] {
    COMPACTION_POLICY_KEYS
}

pub fn compaction_policy_to_vm_value(policy: &CompactionPolicy) -> VmValue {
    let mut map = BTreeMap::new();
    if let Some(instructions) = policy.instructions.as_ref() {
        map.insert(
            "instructions".to_string(),
            VmValue::String(std::sync::Arc::from(instructions.clone())),
        );
    }
    if let Some(mode) = policy.mode.as_ref() {
        map.insert(
            "mode".to_string(),
            VmValue::String(std::sync::Arc::from(mode.clone())),
        );
    }
    if let Some(scope) = policy.scope.as_ref() {
        map.insert(
            "scope".to_string(),
            VmValue::String(std::sync::Arc::from(scope.clone())),
        );
    }
    map.insert(
        "preserve".to_string(),
        VmValue::List(std::sync::Arc::new(
            policy
                .preserve
                .iter()
                .map(|item| VmValue::String(std::sync::Arc::from(item.clone())))
                .collect(),
        )),
    );
    map.insert(
        "drop".to_string(),
        VmValue::List(std::sync::Arc::new(
            policy
                .drop_items
                .iter()
                .map(|item| VmValue::String(std::sync::Arc::from(item.clone())))
                .collect(),
        )),
    );
    if let Some(extend_default_instructions) = policy.extend_default_instructions {
        map.insert(
            "extend_default_instructions".to_string(),
            VmValue::Bool(extend_default_instructions),
        );
    }
    if let Some(author) = policy.author.as_ref() {
        map.insert(
            "author".to_string(),
            VmValue::String(std::sync::Arc::from(author.clone())),
        );
    }
    VmValue::Dict(std::sync::Arc::new(map))
}

pub fn parse_compaction_policy_options(
    options: Option<&BTreeMap<String, VmValue>>,
    builtin: &str,
) -> Result<CompactionPolicy, VmError> {
    let mut policy = options
        .and_then(|map| {
            map.get("policy")
                .or_else(|| map.get("compaction_policy"))
                .or_else(|| map.get("compaction_request"))
        })
        .map(|value| parse_compaction_policy_value(value, builtin))
        .transpose()?
        .unwrap_or_default();
    if let Some(options) = options {
        apply_compaction_policy_fields(&mut policy, options, builtin)?;
    }
    Ok(policy)
}

fn parse_compaction_policy_value(
    value: &VmValue,
    builtin: &str,
) -> Result<CompactionPolicy, VmError> {
    match value {
        VmValue::Nil => Ok(CompactionPolicy::default()),
        VmValue::Dict(map) => {
            if let Some(nested) = map
                .get("policy")
                .or_else(|| map.get("compaction_policy"))
                .or_else(|| map.get("compaction_request"))
            {
                let mut policy = parse_compaction_policy_value(nested, builtin)?;
                apply_compaction_policy_fields(&mut policy, map, builtin)?;
                Ok(policy)
            } else {
                let mut policy = CompactionPolicy::default();
                apply_compaction_policy_fields(&mut policy, map, builtin)?;
                Ok(policy)
            }
        }
        other => Err(VmError::Runtime(format!(
            "{builtin}: compaction policy must be a dict or nil, got {}",
            other.type_name()
        ))),
    }
}

fn apply_compaction_policy_fields(
    policy: &mut CompactionPolicy,
    map: &BTreeMap<String, VmValue>,
    builtin: &str,
) -> Result<(), VmError> {
    if let Some(value) = optional_policy_string(map, "instructions", builtin)? {
        policy.instructions = Some(value);
    }
    if let Some(value) = optional_policy_string(map, "mode", builtin)? {
        policy.mode = Some(value);
    }
    if let Some(value) = optional_policy_string(map, "scope", builtin)? {
        policy.scope = Some(value);
    }
    if map.contains_key("preserve") {
        policy.preserve = policy_string_list(map.get("preserve"), builtin, "preserve")?;
    }
    if map.contains_key("drop") {
        policy.drop_items = policy_string_list(map.get("drop"), builtin, "drop")?;
    }
    if let Some(value) = optional_policy_bool(map, "extend_default_instructions", builtin)? {
        policy.extend_default_instructions = Some(value);
    }
    if let Some(value) = optional_policy_string(map, "author", builtin)? {
        policy.author = Some(value);
    }
    Ok(())
}

fn optional_policy_string(
    map: &BTreeMap<String, VmValue>,
    key: &str,
    builtin: &str,
) -> Result<Option<String>, VmError> {
    match map.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: compaction policy `{key}` must be a string, got {}",
            other.type_name()
        ))),
    }
}

fn optional_policy_bool(
    map: &BTreeMap<String, VmValue>,
    key: &str,
    builtin: &str,
) -> Result<Option<bool>, VmError> {
    match map.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Bool(value)) => Ok(Some(*value)),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: compaction policy `{key}` must be a bool, got {}",
            other.type_name()
        ))),
    }
}

fn policy_string_list(
    value: Option<&VmValue>,
    builtin: &str,
    key: &str,
) -> Result<Vec<String>, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(Vec::new()),
        Some(VmValue::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                Ok(Vec::new())
            } else {
                Ok(vec![trimmed.to_string()])
            }
        }
        Some(VmValue::List(items)) => items
            .iter()
            .map(|item| match item {
                VmValue::String(text) => Ok(text.trim().to_string()),
                other => Err(VmError::Runtime(format!(
                    "{builtin}: compaction policy `{key}` entries must be strings, got {}",
                    other.type_name()
                ))),
            })
            .filter_map(|result| match result {
                Ok(value) if value.is_empty() => None,
                other => Some(other),
            })
            .collect(),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: compaction policy `{key}` must be a string or list, got {}",
            other.type_name()
        ))),
    }
}

pub fn compaction_policy_metadata_fields(
    policy: &CompactionPolicy,
) -> Vec<(&'static str, serde_json::Value)> {
    let mut fields = vec![(
        "instruction_mode",
        serde_json::Value::String(policy.instruction_mode().to_string()),
    )];
    if let Some(source) = policy.instruction_source() {
        fields.push((
            "instruction_source",
            serde_json::Value::String(source.to_string()),
        ));
    }
    if let Some(policy_json) = policy.metadata_json() {
        fields.push(("compaction_policy", policy_json));
    }
    fields
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
    /// Number of earliest messages to keep verbatim before the compacted
    /// summary. The system prompt is not part of this list and is always
    /// preserved separately by the caller.
    pub keep_first: usize,
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
    /// Pending reminders supplied to `custom_compactor` as a second
    /// argument. Built-in compaction strategies decide reminder retention
    /// before rebuilding the transcript, so they do not consume this list.
    pub custom_compactor_reminders: Vec<VmValue>,
    /// Optional callback for domain-specific per-message masking during
    /// observation mask compaction. Called with a list of archived messages,
    /// returns a list of `Option<String>` — `Some(masked)` to override the
    /// default mask for that message, `None` to use the default.
    /// This lets the host (e.g. an IDE or cloud runner) inject AST outlines,
    /// file summaries, etc. without putting language-specific logic in Harn.
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
    /// User-facing policy label for replay and observability. This can be
    /// broader than the engine strategy, e.g. `hybrid` lowers to LLM
    /// summarization plus truncate fallback.
    pub policy_strategy: String,
    /// Strategy to try when the primary strategy fails. Budget-pressure
    /// compaction uses this to keep the session within its hard cap even when
    /// an LLM summarizer is unavailable.
    pub fallback_strategy: Option<CompactStrategy>,
    /// Host/user-supplied instructions that guide compaction without
    /// becoming part of the compacted transcript unless `scope` explicitly
    /// asks for model-visible policy text.
    pub policy: CompactionPolicy,
}

impl Default for AutoCompactConfig {
    fn default() -> Self {
        Self {
            keep_first: 0,
            token_threshold: 48_000,
            tool_output_max_chars: 16_000,
            keep_last: 12,
            compact_strategy: CompactStrategy::ObservationMask,
            hard_limit_tokens: None,
            hard_limit_strategy: CompactStrategy::Llm,
            custom_compactor: None,
            custom_compactor_reminders: Vec::new(),
            mask_callback: None,
            compress_callback: None,
            summarize_prompt: None,
            policy_strategy: compact_strategy_name(&CompactStrategy::ObservationMask).to_string(),
            fallback_strategy: None,
            policy: CompactionPolicy::default(),
        }
    }
}

/// Estimate token count from a list of JSON messages (chars / 4 heuristic).
pub fn estimate_message_tokens(messages: &[serde_json::Value]) -> usize {
    messages.iter().map(estimate_message_chars).sum::<usize>() / 4
}

fn estimate_message_chars(message: &serde_json::Value) -> usize {
    let mut total = message
        .get("content")
        .map(estimate_content_chars)
        .unwrap_or_default();
    if let Some(reasoning) = message.get("reasoning") {
        total += estimate_content_chars(reasoning);
    }
    if let Some(tool_calls) = message.get("tool_calls") {
        total += estimate_content_chars(tool_calls);
    }
    total
}

fn estimate_content_chars(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::String(text) => text.len(),
        serde_json::Value::Array(items) => items.iter().map(estimate_content_chars).sum(),
        serde_json::Value::Object(map) => map.values().map(estimate_content_chars).sum(),
        serde_json::Value::Null => 0,
        other => other.to_string().len(),
    }
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
    policy: &CompactionPolicy,
) -> Result<String, VmError> {
    let mut compact_opts = llm_opts.clone();
    let formatted = format_compaction_messages(old_messages);
    compact_opts.system = None;
    compact_opts.transcript_summary = None;
    compact_opts.native_tools = None;
    compact_opts.tool_choice = None;
    compact_opts.output_format = crate::llm::api::OutputFormat::Text;
    compact_opts.response_format = None;
    compact_opts.json_schema = None;
    compact_opts.output_schema = None;
    let prompt =
        render_llm_compaction_prompt(summarize_prompt, &formatted, archived_count, policy)?;
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
    policy: &CompactionPolicy,
) -> Result<String, VmError> {
    if policy.has_prompt_directives() && policy.extend_default_instructions == Some(false) {
        return render_replacement_compaction_prompt(policy, formatted, archived_count);
    }
    let mut bindings = BTreeMap::new();
    bindings.insert(
        "formatted_messages".to_string(),
        VmValue::String(std::sync::Arc::from(formatted.to_string())),
    );
    bindings.insert(
        "archived_count".to_string(),
        VmValue::Int(archived_count as i64),
    );
    let Some(path) = summarize_prompt.filter(|path| !path.trim().is_empty()) else {
        let prompt = crate::stdlib::template::render_stdlib_prompt_asset(
            "orchestration/prompts/compaction_summary.harn.prompt",
            Some(&bindings),
        )?;
        return Ok(extend_compaction_prompt(prompt, policy));
    };

    let asset = crate::stdlib::template::TemplateAsset::render_target(path)
        .map_err(|error| VmError::Runtime(format!("compaction summarize_prompt: {error}")))?;
    let prompt = crate::stdlib::template::render_asset_result(&asset, Some(&bindings))
        .map_err(VmError::from)?;
    Ok(extend_compaction_prompt(prompt, policy))
}

fn render_replacement_compaction_prompt(
    policy: &CompactionPolicy,
    formatted: &str,
    archived_count: usize,
) -> Result<String, VmError> {
    let directives = policy.prompt_directives().unwrap_or_default();
    let mut bindings = BTreeMap::new();
    bindings.insert(
        "directives".to_string(),
        VmValue::String(std::sync::Arc::from(directives)),
    );
    bindings.insert(
        "formatted_messages".to_string(),
        VmValue::String(std::sync::Arc::from(formatted.to_string())),
    );
    bindings.insert(
        "archived_count".to_string(),
        VmValue::Int(archived_count as i64),
    );
    crate::stdlib::template::render_stdlib_prompt_asset(
        "orchestration/prompts/compaction_policy_replacement.harn.prompt",
        Some(&bindings),
    )
}

fn extend_compaction_prompt(mut prompt: String, policy: &CompactionPolicy) -> String {
    let Some(directives) = policy.prompt_directives() else {
        return prompt;
    };
    prompt.push_str(
        "\n\nAdditional compaction instructions: use these directives to shape the summary, but do not quote this section unless it explicitly requests a model-visible note.\n",
    );
    prompt.push_str(&directives);
    prompt
}

async fn custom_compaction_summary(
    ctx: Option<&AsyncBuiltinCtx>,
    old_messages: &[serde_json::Value],
    archived_count: usize,
    callback: &VmValue,
    reminders: &[VmValue],
    policy: &CompactionPolicy,
) -> Result<String, VmError> {
    let Some(VmValue::Closure(closure)) = Some(callback.clone()) else {
        return Err(VmError::Runtime(
            "compact_callback must be a closure when compact_strategy is 'custom'".to_string(),
        ));
    };
    let Some(ctx) = ctx else {
        return Err(VmError::Runtime(
            "custom transcript compaction requires an async builtin VM context".to_string(),
        ));
    };
    let mut vm = ctx.child_vm();
    let messages_vm = VmValue::List(std::sync::Arc::new(
        old_messages
            .iter()
            .map(crate::stdlib::json_to_vm_value)
            .collect(),
    ));
    let result = if policy.has_metadata()
        && (closure.func.params.len() >= 3 || closure.func.has_rest_param)
    {
        let reminders_vm = VmValue::List(std::sync::Arc::new(reminders.to_vec()));
        let policy_vm = compaction_policy_to_vm_value(policy);
        vm.call_closure_pub(&closure, &[messages_vm, reminders_vm, policy_vm])
            .await
    } else if closure.func.params.len() >= 2 || closure.func.has_rest_param {
        let reminders_vm = VmValue::List(std::sync::Arc::new(reminders.to_vec()));
        vm.call_closure_pub(&closure, &[messages_vm, reminders_vm])
            .await
    } else {
        vm.call_closure_pub(&closure, &[messages_vm]).await
    };
    let summary = compact_summary_text_from_value(&result?)?;
    ctx.forward_output(&vm.take_output());
    if summary.trim().is_empty() {
        Ok(truncate_compaction_summary(old_messages, archived_count))
    } else {
        Ok(format!(
            "[auto-compacted {archived_count} older messages]\n{summary}"
        ))
    }
}

/// Marker the host emits inside a tool-output (or message) body to pin its
/// live grounding — the current file view and just-edited window — so it
/// survives a compaction pass. Burin renders this literal substring inside
/// markdown headings (e.g. `## Exact current file text [no-compact]`,
/// `## Edited region now reads (...) [no-compact]`) in
/// `lib/tools/result-format.harn`. Compaction matches the substring; it does
/// not invent a new vocabulary.
pub(crate) const NO_COMPACT_MARKER: &str = "[no-compact]";

/// Upper bound on how many of the most-recent pinned segments survive a
/// compaction pass verbatim. A pin that could never be evicted would let a
/// long session accumulate unbounded pinned snapshots (e.g. one edited-window
/// per edit) and eventually overflow the context window — defeating the
/// purpose of compaction. Keeping only the latest few preserves the agent's
/// *current* grounding (the file it is editing now, emitted as the exact-text
/// block plus the numbered-lines block in one or two adjacent outputs) while
/// letting stale duplicates from earlier in the session compact normally.
pub(crate) const MAX_PINNED_SEGMENTS: usize = 3;

/// Whether a content body carries the host's `[no-compact]` pin marker.
fn is_pinned_content(content: &str) -> bool {
    content.contains(NO_COMPACT_MARKER)
}

/// Compute the set of message indices into `messages` that are pinned AND fall
/// within the most-recent [`MAX_PINNED_SEGMENTS`] pinned bodies. Older pinned
/// bodies are intentionally excluded so they compact normally (the bound).
/// `content_of` extracts the body text to inspect for each message.
fn latest_pinned_indices<'a, F>(
    messages: impl Iterator<Item = &'a serde_json::Value>,
    content_of: F,
) -> std::collections::HashSet<usize>
where
    F: Fn(&serde_json::Value) -> Option<&str>,
{
    // Walk newest-first, collecting up to MAX_PINNED_SEGMENTS pinned indices.
    let pinned: Vec<usize> = messages
        .enumerate()
        .filter(|(_, msg)| content_of(msg).is_some_and(is_pinned_content))
        .map(|(idx, _)| idx)
        .collect();
    pinned.into_iter().rev().take(MAX_PINNED_SEGMENTS).collect()
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
    // Pin the agent's most-recent live grounding: any archived body carrying
    // the host's `[no-compact]` marker (the current file view / edited window)
    // survives masking verbatim, bounded to the latest MAX_PINNED_SEGMENTS so a
    // long session's stale snapshots still compact.
    let pinned = latest_pinned_indices(old_messages.iter(), |msg| {
        msg.get("content").and_then(|v| v.as_str())
    });
    for (idx, msg) in old_messages.iter().enumerate() {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let content = msg
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if content.is_empty() {
            continue;
        }
        if pinned.contains(&idx) {
            parts.push(format!("[{role}] {content}"));
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
    ctx: Option<&AsyncBuiltinCtx>,
    callback: &VmValue,
    old_messages: &[serde_json::Value],
) -> Result<Vec<Option<String>>, VmError> {
    let VmValue::Closure(closure) = callback.clone() else {
        return Err(VmError::Runtime(
            "mask_callback must be a closure".to_string(),
        ));
    };
    let Some(ctx) = ctx else {
        return Err(VmError::Runtime(
            "mask_callback requires an async builtin VM context".to_string(),
        ));
    };
    let mut vm = ctx.child_vm();
    let messages_vm = VmValue::List(std::sync::Arc::new(
        old_messages
            .iter()
            .map(crate::stdlib::json_to_vm_value)
            .collect(),
    ));
    let result = vm.call_closure_pub(&closure, &[messages_vm]).await?;
    ctx.forward_output(&vm.take_output());
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

/// Rewrite each tool-result message in `messages` whose content exceeds
/// `config.tool_output_max_chars`, using `config.compress_callback` when set
/// (and a VM context is available) else the deterministic
/// [`microcompact_tool_output`]. Only the `content` text is replaced; the
/// message's `role`/`tool_call_id` are left untouched so tool-call pairing is
/// preserved. A `tool_output_max_chars` of 0 disables the pass.
async fn clamp_tool_outputs(
    ctx: Option<&AsyncBuiltinCtx>,
    messages: &mut [serde_json::Value],
    config: &AutoCompactConfig,
) -> Result<(), VmError> {
    if config.tool_output_max_chars == 0 {
        return Ok(());
    }
    // Exempt the most-recent pinned tool-outputs (those carrying the host's
    // `[no-compact]` marker) from length-clamping so the agent's live file view
    // stays intact. Bounded to the latest MAX_PINNED_SEGMENTS so older pinned
    // snapshots in the kept window still clamp and can't blow the budget.
    let pinned = latest_pinned_indices(messages.iter(), |msg| {
        if msg.get("role").and_then(|role| role.as_str()) == Some("tool") {
            msg.get("content").and_then(|content| content.as_str())
        } else {
            None
        }
    });
    for (idx, message) in messages.iter_mut().enumerate() {
        if message.get("role").and_then(|role| role.as_str()) != Some("tool") {
            continue;
        }
        let Some(content) = message.get("content").and_then(|content| content.as_str()) else {
            continue;
        };
        if content.len() <= config.tool_output_max_chars {
            continue;
        }
        if pinned.contains(&idx) {
            continue;
        }
        let content = content.to_string();
        let replacement = match (config.compress_callback.as_ref(), ctx) {
            (Some(callback), Some(ctx)) => {
                invoke_compress_callback(ctx, callback, &content, config.tool_output_max_chars)
                    .await?
            }
            _ => microcompact_tool_output(&content, config.tool_output_max_chars),
        };
        message["content"] = serde_json::Value::String(replacement);
    }
    Ok(())
}

/// Invoke `compress_callback(content, max_chars)` to replace one oversized
/// tool-output body, mirroring [`invoke_mask_callback`]'s child-VM closure
/// invocation. A non-string return falls back to the deterministic primitive.
async fn invoke_compress_callback(
    ctx: &AsyncBuiltinCtx,
    callback: &VmValue,
    content: &str,
    max_chars: usize,
) -> Result<String, VmError> {
    let VmValue::Closure(closure) = callback.clone() else {
        return Err(VmError::Runtime(
            "compress_callback must be a closure".to_string(),
        ));
    };
    let mut vm = ctx.child_vm();
    let args = [
        VmValue::String(std::sync::Arc::from(content)),
        VmValue::Int(max_chars as i64),
    ];
    let result = vm.call_closure_pub(&closure, &args).await?;
    ctx.forward_output(&vm.take_output());
    match result {
        VmValue::String(text) => Ok(text.to_string()),
        _ => Ok(microcompact_tool_output(content, max_chars)),
    }
}

#[derive(Clone, Copy)]
struct CompactionStrategyInputs<'a> {
    ctx: Option<&'a AsyncBuiltinCtx>,
    strategy: &'a CompactStrategy,
    old_messages: &'a [serde_json::Value],
    archived_count: usize,
    llm_opts: Option<&'a crate::llm::api::LlmCallOptions>,
    custom_compactor: Option<&'a VmValue>,
    custom_compactor_reminders: &'a [VmValue],
    mask_callback: Option<&'a VmValue>,
    summarize_prompt: Option<&'a str>,
    policy: &'a CompactionPolicy,
}

/// Apply a single compaction strategy to a list of archived messages.
async fn apply_compaction_strategy(input: CompactionStrategyInputs<'_>) -> Result<String, VmError> {
    let CompactionStrategyInputs {
        strategy,
        old_messages,
        archived_count,
        llm_opts,
        custom_compactor,
        custom_compactor_reminders,
        mask_callback,
        summarize_prompt,
        policy,
        ctx,
    } = input;
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
                policy,
            )
            .await
        }
        CompactStrategy::Custom => {
            custom_compaction_summary(
                ctx,
                old_messages,
                archived_count,
                custom_compactor.ok_or_else(|| {
                    VmError::Runtime(
                        "compact_callback is required when compact_strategy is 'custom'"
                            .to_string(),
                    )
                })?,
                custom_compactor_reminders,
                policy,
            )
            .await
        }
        CompactStrategy::ObservationMask => {
            let mask_results = if let Some(cb) = mask_callback {
                Some(invoke_mask_callback(ctx, cb, old_messages).await?)
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

async fn apply_compaction_strategy_with_fallback(
    input: CompactionStrategyInputs<'_>,
    fallback_strategy: Option<&CompactStrategy>,
) -> Result<(String, CompactStrategy), VmError> {
    match apply_compaction_strategy(input).await {
        Ok(summary) => Ok((summary, input.strategy.clone())),
        Err(primary_error) => {
            let Some(fallback) = fallback_strategy.filter(|fallback| *fallback != input.strategy)
            else {
                return Err(primary_error);
            };
            let fallback_input = CompactionStrategyInputs {
                strategy: fallback,
                ..input
            };
            apply_compaction_strategy(fallback_input)
                .await
                .map(|summary| (summary, fallback.clone()))
        }
    }
}

pub(crate) struct AutoCompactResult {
    pub summary: String,
    pub strategy: CompactStrategy,
}

/// Auto-compact a message list in place using two-tier compaction.
#[cfg(test)]
pub(crate) async fn auto_compact_messages_with_result(
    messages: &mut Vec<serde_json::Value>,
    config: &AutoCompactConfig,
    llm_opts: Option<&crate::llm::api::LlmCallOptions>,
) -> Result<Option<AutoCompactResult>, VmError> {
    auto_compact_messages_with_result_with_ctx(None, messages, config, llm_opts).await
}

pub(crate) async fn auto_compact_messages_with_result_with_ctx(
    ctx: Option<&AsyncBuiltinCtx>,
    messages: &mut Vec<serde_json::Value>,
    config: &AutoCompactConfig,
    llm_opts: Option<&crate::llm::api::LlmCallOptions>,
) -> Result<Option<AutoCompactResult>, VmError> {
    if config.token_threshold > 0 && estimate_message_tokens(messages) <= config.token_threshold {
        return Ok(None);
    }
    if messages.len() <= config.keep_first.saturating_add(config.keep_last) {
        return Ok(None);
    }
    let compact_start = config.keep_first.min(messages.len());
    let original_split = messages.len().saturating_sub(config.keep_last);
    let mut split_at = original_split;
    // Snap back to a user-role boundary so the kept suffix begins at a clean
    // turn. OpenAI-compatible APIs reject tool results orphaned from their
    // assistant request, so splitting mid-turn corrupts the transcript.
    while split_at > compact_start
        && split_at < messages.len()
        && messages[split_at]
            .get("role")
            .and_then(|r| r.as_str())
            .is_none_or(|r| r != "user")
    {
        split_at -= 1;
    }
    // Fall back to the naive split (e.g. tool-heavy transcripts with the sole
    // user message at index 0) rather than skipping compaction entirely.
    if split_at == compact_start {
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
            .filter(|boundary| *boundary > compact_start)
        {
            split_at = boundary;
        }
    }
    if split_at <= compact_start {
        return Ok(None);
    }
    let old_messages: Vec<_> = messages.drain(compact_start..split_at).collect();
    let archived_count = old_messages.len();

    // Clamp oversized tool-result bodies in the *kept* window so the live
    // context honors the policy's `tool_output_max_chars` (and the
    // `compress_callback` override), not just the archived/summarized window —
    // the two config fields were previously parsed and defaulted but never
    // applied here. Runs before the hard-limit estimate so tier-2 escalation
    // keys off the post-clamp size. Only the text body is rewritten; `role`
    // and `tool_call_id` are preserved so tool_call/tool_result pairing stays
    // intact.
    clamp_tool_outputs(ctx, messages, config).await?;

    let (mut summary, mut strategy) = apply_compaction_strategy_with_fallback(
        CompactionStrategyInputs {
            ctx,
            strategy: &config.compact_strategy,
            old_messages: &old_messages,
            archived_count,
            llm_opts,
            custom_compactor: config.custom_compactor.as_ref(),
            custom_compactor_reminders: &config.custom_compactor_reminders,
            mask_callback: config.mask_callback.as_ref(),
            summarize_prompt: config.summarize_prompt.as_deref(),
            policy: &config.policy,
        },
        config.fallback_strategy.as_ref(),
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
            let (hard_limit_summary, hard_limit_strategy) =
                apply_compaction_strategy_with_fallback(
                    CompactionStrategyInputs {
                        ctx,
                        strategy: &config.hard_limit_strategy,
                        old_messages: &tier1_as_messages,
                        archived_count,
                        llm_opts,
                        custom_compactor: config.custom_compactor.as_ref(),
                        custom_compactor_reminders: &config.custom_compactor_reminders,
                        mask_callback: None,
                        summarize_prompt: config.summarize_prompt.as_deref(),
                        policy: &config.policy,
                    },
                    config.fallback_strategy.as_ref(),
                )
                .await?;
            summary = hard_limit_summary;
            strategy = hard_limit_strategy;
        }
    }

    summary = apply_model_visible_policy(summary, &config.policy);

    messages.insert(
        compact_start,
        serde_json::json!({
            "role": "user",
            "content": summary,
        }),
    );
    Ok(Some(AutoCompactResult { summary, strategy }))
}

/// Auto-compact a message list in place using two-tier compaction.
#[cfg(test)]
pub(crate) async fn auto_compact_messages(
    messages: &mut Vec<serde_json::Value>,
    config: &AutoCompactConfig,
    llm_opts: Option<&crate::llm::api::LlmCallOptions>,
) -> Result<Option<String>, VmError> {
    Ok(
        auto_compact_messages_with_result(messages, config, llm_opts)
            .await?
            .map(|result| result.summary),
    )
}

fn apply_model_visible_policy(mut summary: String, policy: &CompactionPolicy) -> String {
    if !policy.is_model_visible_scope() {
        return summary;
    }
    let Some(directives) = policy.prompt_directives() else {
        return summary;
    };
    summary.push_str("\n\n[compaction instructions]\n");
    summary.push_str(&directives);
    summary
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
            .map(|i| format!("line {i:02} content here"))
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
    fn token_estimate_counts_structured_message_content() {
        let text = "x".repeat(400);
        let messages = vec![serde_json::json!({
            "role": "user",
            "content": [
                {"type": "text", "text": text},
                {"type": "input_text", "text": "tail"},
            ],
            "reasoning": {"text": "scratch"},
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {"name": "read", "arguments": "{\"path\":\"src/main.rs\"}"}
            }],
        })];

        assert!(
            estimate_message_tokens(&messages) >= 100,
            "structured content must not count as zero"
        );
    }

    #[test]
    fn compaction_policy_instructions_extend_by_default() {
        let policy = CompactionPolicy {
            instructions: Some("Keep the failing test names.".to_string()),
            ..Default::default()
        };
        let prompt = render_llm_compaction_prompt(None, "[user] old context", 1, &policy)
            .expect("prompt renders");

        assert_eq!(policy.instruction_mode(), "extend");
        assert!(prompt.contains("Preserve goals, constraints"));
        assert!(prompt.contains("Additional compaction instructions"));
        assert!(prompt.contains("Keep the failing test names."));
    }

    #[test]
    fn compaction_policy_can_replace_default_instructions() {
        let policy = CompactionPolicy {
            instructions: Some("Only keep repro steps.".to_string()),
            extend_default_instructions: Some(false),
            ..Default::default()
        };
        let prompt = render_llm_compaction_prompt(None, "[user] old context", 1, &policy)
            .expect("prompt renders");

        assert_eq!(policy.instruction_mode(), "replace");
        assert!(prompt.contains("according to these instructions"));
        assert!(prompt.contains("Only keep repro steps."));
        assert!(!prompt.contains("Preserve goals, constraints"));
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
            token_threshold: 1,
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

    #[test]
    fn auto_compact_clamps_oversized_tool_output_to_max_chars() {
        // A large tool result in the *kept* window must be clamped to honor
        // `tool_output_max_chars` — the engine previously ignored that config.
        let big = "x".repeat(4000);
        let big_len = big.len();
        let mut messages = vec![
            serde_json::json!({"role": "user", "content": "old task"}),
            serde_json::json!({"role": "assistant", "content": "old reply"}),
            serde_json::json!({"role": "user", "content": "new task"}),
            serde_json::json!({"role": "assistant", "content": "calling tool"}),
            serde_json::json!({"role": "tool", "tool_call_id": "call_1", "content": big}),
        ];
        let config = AutoCompactConfig {
            token_threshold: 1,
            keep_last: 2,
            tool_output_max_chars: 500,
            ..Default::default()
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let result = runtime
            .block_on(auto_compact_messages(&mut messages, &config, None))
            .expect("compaction succeeds");
        assert!(result.is_some(), "compaction should trigger");

        let tool_msg = messages
            .iter()
            .find(|message| message["role"] == "tool")
            .expect("tool message kept in window");
        // Pairing preserved...
        assert_eq!(tool_msg["tool_call_id"], "call_1");
        // ...and the oversized body was clamped well below its original size.
        let content = tool_msg["content"].as_str().expect("string content");
        assert!(
            content.len() < big_len,
            "tool output should be clamped: {} vs {}",
            content.len(),
            big_len
        );
        assert!(content.len() < 2000, "clamped near tool_output_max_chars");
    }

    /// (1) A pinned tool-output survives an observation-mask pass that evicts
    /// (masks) the unpinned verbose outputs around it.
    #[test]
    fn observation_mask_preserves_pinned_live_file_view() {
        let pinned_body = format!(
            "## Edited region now reads (line 42, ±6 context) {}\n```\n{}\n```",
            NO_COMPACT_MARKER,
            (0..40)
                .map(|i| format!("   {i}  let x = compute({i});"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let verbose_unpinned = (0..60)
            .map(|i| format!("verbose scan output line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        // These are the ARCHIVED messages handed to the mask pass.
        let archived = vec![
            serde_json::json!({"role": "user", "content": verbose_unpinned}),
            serde_json::json!({"role": "user", "content": pinned_body}),
        ];
        let summary = observation_mask_compaction(&archived, archived.len());
        // Pinned live file view survives verbatim.
        assert!(
            summary.contains("Edited region now reads"),
            "pinned heading survived: {summary}"
        );
        assert!(
            summary.contains("let x = compute(39);"),
            "pinned body survived verbatim"
        );
        // The unpinned verbose neighbor was masked.
        assert!(summary.contains("masked]"), "unpinned output was masked");
        assert!(!summary.contains("verbose scan output line 30"));
    }

    /// (2) A pinned large tool-output is NOT clamped, while an unpinned one of
    /// the same size IS.
    #[test]
    fn clamp_exempts_pinned_tool_output() {
        let pinned_big = format!(
            "## Exact current file text {}\n{}",
            NO_COMPACT_MARKER,
            "x".repeat(4000)
        );
        let pinned_len = pinned_big.len();
        let unpinned_big = "y".repeat(4000);
        let unpinned_len = unpinned_big.len();
        let mut messages = vec![
            serde_json::json!({"role": "user", "content": "old task"}),
            serde_json::json!({"role": "assistant", "content": "reply"}),
            serde_json::json!({"role": "user", "content": "new task"}),
            serde_json::json!({"role": "assistant", "content": "calling tools"}),
            serde_json::json!({"role": "tool", "tool_call_id": "c0", "content": unpinned_big}),
            serde_json::json!({"role": "tool", "tool_call_id": "c1", "content": pinned_big}),
            serde_json::json!({"role": "user", "content": "continue"}),
        ];
        let config = AutoCompactConfig {
            token_threshold: 1,
            keep_last: 4,
            tool_output_max_chars: 500,
            ..Default::default()
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime
            .block_on(auto_compact_messages(&mut messages, &config, None))
            .expect("compaction succeeds");

        let pinned_msg = messages
            .iter()
            .find(|m| m["tool_call_id"] == "c1")
            .expect("pinned tool message kept");
        assert_eq!(
            pinned_msg["content"].as_str().map(str::len),
            Some(pinned_len),
            "pinned output must be intact (unclamped)"
        );
        let unpinned_msg = messages
            .iter()
            .find(|m| m["tool_call_id"] == "c0")
            .expect("unpinned tool message kept");
        assert!(
            unpinned_msg["content"].as_str().map(str::len).unwrap() < unpinned_len,
            "unpinned output of the same size must be clamped"
        );
    }

    /// (3) Bounded policy: with MANY pinned outputs, only the latest
    /// MAX_PINNED_SEGMENTS survive verbatim; older pinned duplicates compact —
    /// so the pin can't prevent all compaction (and can't overflow the window
    /// on a very long session).
    #[test]
    fn pin_bound_keeps_only_latest_segments() {
        // Build 6 distinct pinned, oversized edited-window snapshots
        // (gen 0 = oldest .. gen 5 = newest), each tagged with the marker and
        // long enough that masking would otherwise truncate it.
        let make = |gen: usize| {
            let body = (0..40)
                .map(|i| format!("marker-gen-{gen} body line {i}"))
                .collect::<Vec<_>>()
                .join("\n");
            serde_json::json!({
                "role": "user",
                "content": format!(
                    "## Edited region now reads (gen {gen}) {}\n{}",
                    NO_COMPACT_MARKER, body
                ),
            })
        };
        let archived: Vec<_> = (0..6).map(make).collect();

        // Unit-level: the index selection keeps exactly the latest N.
        let pinned = latest_pinned_indices(archived.iter(), |m| {
            m.get("content").and_then(|c| c.as_str())
        });
        assert_eq!(
            pinned.len(),
            MAX_PINNED_SEGMENTS,
            "only the latest MAX_PINNED_SEGMENTS are pinned"
        );
        assert!(pinned.contains(&5) && pinned.contains(&4) && pinned.contains(&3));
        assert!(!pinned.contains(&0) && !pinned.contains(&1) && !pinned.contains(&2));

        // End-to-end through the mask pass: the 3 newest snapshots survive
        // verbatim; the 3 oldest are masked, proving the pin cannot defeat all
        // compaction.
        let summary = observation_mask_compaction(&archived, archived.len());
        assert!(
            summary.contains("marker-gen-5")
                && summary.contains("marker-gen-4")
                && summary.contains("marker-gen-3"),
            "latest {MAX_PINNED_SEGMENTS} pinned snapshots survive verbatim: {summary}"
        );
        assert!(
            !summary.contains("marker-gen-0")
                && !summary.contains("marker-gen-1")
                && !summary.contains("marker-gen-2"),
            "older pinned snapshots are masked (bound enforced)"
        );
        assert!(summary.contains("masked]"), "older snapshots were masked");
    }

    /// (4) Regression: with NO pins, compaction behaves exactly as before.
    #[test]
    fn no_pins_preserves_prior_clamp_behavior() {
        let big = "x".repeat(4000);
        let big_len = big.len();
        let mut messages = vec![
            serde_json::json!({"role": "user", "content": "old task"}),
            serde_json::json!({"role": "assistant", "content": "old reply"}),
            serde_json::json!({"role": "user", "content": "new task"}),
            serde_json::json!({"role": "assistant", "content": "calling tool"}),
            serde_json::json!({"role": "tool", "tool_call_id": "call_1", "content": big}),
        ];
        let config = AutoCompactConfig {
            token_threshold: 1,
            keep_last: 2,
            tool_output_max_chars: 500,
            ..Default::default()
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let result = runtime
            .block_on(auto_compact_messages(&mut messages, &config, None))
            .expect("compaction succeeds");
        assert!(result.is_some());
        let tool_msg = messages
            .iter()
            .find(|m| m["role"] == "tool")
            .expect("tool kept");
        let content = tool_msg["content"].as_str().expect("string content");
        assert!(content.len() < big_len, "unpinned output clamped as before");
        assert!(content.len() < 2000, "clamped near tool_output_max_chars");
    }
}
