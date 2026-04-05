use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::{cell::RefCell, thread_local};

use serde::{Deserialize, Serialize};

use crate::llm::{extract_llm_options, vm_call_llm_full, vm_value_to_json};
use crate::value::{VmError, VmValue};

fn now_rfc3339() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{ts}")
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::now_v7())
}

fn default_run_dir() -> PathBuf {
    std::env::var("HARN_RUN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".harn-runs"))
}

thread_local! {
    static EXECUTION_POLICY_STACK: RefCell<Vec<CapabilityPolicy>> = const { RefCell::new(Vec::new()) };
    static TOOL_HOOKS: RefCell<Vec<ToolHook>> = const { RefCell::new(Vec::new()) };
    static CURRENT_MUTATION_SESSION: RefCell<Option<MutationSessionRecord>> = const { RefCell::new(None) };
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct MutationSessionRecord {
    pub session_id: String,
    pub parent_session_id: Option<String>,
    pub run_id: Option<String>,
    pub worker_id: Option<String>,
    pub execution_kind: Option<String>,
    pub mutation_scope: String,
    pub approval_mode: String,
}

impl MutationSessionRecord {
    pub fn normalize(mut self) -> Self {
        if self.session_id.is_empty() {
            self.session_id = new_id("session");
        }
        if self.mutation_scope.is_empty() {
            self.mutation_scope = "read_only".to_string();
        }
        if self.approval_mode.is_empty() {
            self.approval_mode = "host_enforced".to_string();
        }
        self
    }
}

pub fn install_current_mutation_session(session: Option<MutationSessionRecord>) {
    CURRENT_MUTATION_SESSION.with(|slot| {
        *slot.borrow_mut() = session.map(MutationSessionRecord::normalize);
    });
}

pub fn current_mutation_session() -> Option<MutationSessionRecord> {
    CURRENT_MUTATION_SESSION.with(|slot| slot.borrow().clone())
}

// ── Tool lifecycle hooks ──────────────────────────────────────────────

/// Action returned by a PreToolUse hook.
#[derive(Clone, Debug)]
pub enum PreToolAction {
    /// Allow the tool call to proceed unchanged.
    Allow,
    /// Deny the tool call with an explanation.
    Deny(String),
    /// Allow but replace the arguments.
    Modify(serde_json::Value),
}

/// Action returned by a PostToolUse hook.
#[derive(Clone, Debug)]
pub enum PostToolAction {
    /// Pass the result through unchanged.
    Pass,
    /// Replace the result text.
    Modify(String),
}

/// Callback types for tool lifecycle hooks.
pub type PreToolHookFn = Rc<dyn Fn(&str, &serde_json::Value) -> PreToolAction>;
pub type PostToolHookFn = Rc<dyn Fn(&str, &str) -> PostToolAction>;

/// A registered tool hook with a name pattern and callbacks.
#[derive(Clone)]
pub struct ToolHook {
    /// Glob-style pattern matched against tool names (e.g. `"*"`, `"exec*"`, `"read_file"`).
    pub pattern: String,
    /// Called before tool execution. Return `Deny` to reject, `Modify` to rewrite args.
    pub pre: Option<PreToolHookFn>,
    /// Called after tool execution with the result text. Return `Modify` to rewrite.
    pub post: Option<PostToolHookFn>,
}

impl std::fmt::Debug for ToolHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolHook")
            .field("pattern", &self.pattern)
            .field("has_pre", &self.pre.is_some())
            .field("has_post", &self.post.is_some())
            .finish()
    }
}

fn glob_match(pattern: &str, name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return name.starts_with(prefix);
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return name.ends_with(suffix);
    }
    pattern == name
}

pub fn register_tool_hook(hook: ToolHook) {
    TOOL_HOOKS.with(|hooks| hooks.borrow_mut().push(hook));
}

pub fn clear_tool_hooks() {
    TOOL_HOOKS.with(|hooks| hooks.borrow_mut().clear());
}

/// Run all matching PreToolUse hooks. Returns the final action.
pub fn run_pre_tool_hooks(tool_name: &str, args: &serde_json::Value) -> PreToolAction {
    TOOL_HOOKS.with(|hooks| {
        let hooks = hooks.borrow();
        let mut current_args = args.clone();
        for hook in hooks.iter() {
            if !glob_match(&hook.pattern, tool_name) {
                continue;
            }
            if let Some(ref pre) = hook.pre {
                match pre(tool_name, &current_args) {
                    PreToolAction::Allow => {}
                    PreToolAction::Deny(reason) => return PreToolAction::Deny(reason),
                    PreToolAction::Modify(new_args) => {
                        current_args = new_args;
                    }
                }
            }
        }
        if current_args != *args {
            PreToolAction::Modify(current_args)
        } else {
            PreToolAction::Allow
        }
    })
}

/// Run all matching PostToolUse hooks. Returns the (possibly modified) result.
pub fn run_post_tool_hooks(tool_name: &str, result: &str) -> String {
    TOOL_HOOKS.with(|hooks| {
        let hooks = hooks.borrow();
        let mut current = result.to_string();
        for hook in hooks.iter() {
            if !glob_match(&hook.pattern, tool_name) {
                continue;
            }
            if let Some(ref post) = hook.post {
                match post(tool_name, &current) {
                    PostToolAction::Pass => {}
                    PostToolAction::Modify(new_result) => {
                        current = new_result;
                    }
                }
            }
        }
        current
    })
}

// ── Auto-compaction ───────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompactStrategy {
    Llm,
    Truncate,
    Custom,
}

pub fn parse_compact_strategy(value: &str) -> Result<CompactStrategy, VmError> {
    match value {
        "llm" => Ok(CompactStrategy::Llm),
        "truncate" => Ok(CompactStrategy::Truncate),
        "custom" => Ok(CompactStrategy::Custom),
        other => Err(VmError::Runtime(format!(
            "unknown compact_strategy '{other}' (expected 'llm', 'truncate', or 'custom')"
        ))),
    }
}

/// Configuration for automatic transcript compaction in agent loops.
#[derive(Clone, Debug)]
pub struct AutoCompactConfig {
    /// Maximum estimated tokens before triggering auto-compaction.
    pub token_threshold: usize,
    /// Maximum character length for a single tool result before microcompaction.
    pub tool_output_max_chars: usize,
    /// Number of recent messages to keep during auto-compaction.
    pub keep_last: usize,
    /// Strategy used to summarize archived messages.
    pub compact_strategy: CompactStrategy,
    /// Optional Harn callback used when `compact_strategy` is `custom`.
    pub custom_compactor: Option<VmValue>,
}

impl Default for AutoCompactConfig {
    fn default() -> Self {
        Self {
            token_threshold: 80_000,
            tool_output_max_chars: 20_000,
            keep_last: 8,
            compact_strategy: CompactStrategy::Llm,
            custom_compactor: None,
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
            // file:line pattern (e.g. "src/main.rs:42:" or "foo.go:10:5:")
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
            // Generic keyword classification. Split into "strong" keywords
            // whose presence alone signals a diagnostic line (regardless of
            // whether the line also has a file:line reference), and "weak"
            // keywords which only count as diagnostic when paired with a
            // file:line (to avoid false positives on narrative prose that
            // happens to contain the word "error" or "expected").
            //
            // These are deliberately generic — not tied to any specific
            // language's test runner output format. Language-specific
            // patterns (Go's "--- FAIL:", pytest's "FAILED tests/", Rust's
            // "thread 'X' panicked at") should be supplied by the pipeline
            // via the `extra_diagnostic_patterns` auto_compact option,
            // which is where language/runner awareness belongs.
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
            let head_end = output.floor_char_boundary(keep);
            let tail_start = output.ceil_char_boundary(output.len() - keep);
            let head = &output[..head_end];
            let tail = &output[tail_start..];
            return format!(
                "{head}\n\n[diagnostic lines preserved]\n{diagnostics}\n\n[... output compacted ...]\n\n{tail}"
            );
        }
    }
    let keep = max_chars / 2;
    let head_end = output.floor_char_boundary(keep);
    let tail_start = output.ceil_char_boundary(output.len() - keep);
    let head = &output[..head_end];
    let tail = &output[tail_start..];
    let snipped = output.len() - max_chars;
    format!("{head}\n\n[... {snipped} characters snipped ...]\n\n{tail}")
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
) -> Result<String, VmError> {
    let mut compact_opts = llm_opts.clone();
    let formatted = format_compaction_messages(old_messages);
    compact_opts.system = None;
    compact_opts.transcript_id = None;
    compact_opts.transcript_summary = None;
    compact_opts.transcript_metadata = None;
    compact_opts.native_tools = None;
    compact_opts.tool_choice = None;
    compact_opts.response_format = None;
    compact_opts.json_schema = None;
    compact_opts.messages = vec![serde_json::json!({
        "role": "user",
        "content": format!(
            "Summarize these archived conversation messages for a follow-on coding agent. Preserve goals, constraints, decisions, completed tool work, unresolved issues, and next actions. Output only the summary text.\n\nArchived message count: {archived_count}\n\nConversation:\n{formatted}"
        ),
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
    let result = vm.call_closure_pub(&closure, &[messages_vm], &[]).await;
    let summary = compact_summary_text_from_value(&result?)?;
    if summary.trim().is_empty() {
        Ok(truncate_compaction_summary(old_messages, archived_count))
    } else {
        Ok(format!(
            "[auto-compacted {archived_count} older messages]\n{summary}"
        ))
    }
}

/// Auto-compact a message list in place: summarize older messages into a
/// note and keep the most recent `keep_last` messages.
pub(crate) async fn auto_compact_messages(
    messages: &mut Vec<serde_json::Value>,
    config: &AutoCompactConfig,
    llm_opts: Option<&crate::llm::api::LlmCallOptions>,
) -> Result<Option<String>, VmError> {
    if messages.len() <= config.keep_last {
        return Ok(None);
    }
    let split_at = messages.len().saturating_sub(config.keep_last);
    let old_messages: Vec<_> = messages.drain(..split_at).collect();
    let archived_count = old_messages.len();
    let summary = match config.compact_strategy {
        CompactStrategy::Truncate => truncate_compaction_summary(&old_messages, archived_count),
        CompactStrategy::Llm => {
            llm_compaction_summary(
                &old_messages,
                archived_count,
                llm_opts.ok_or_else(|| {
                    VmError::Runtime(
                        "LLM transcript compaction requires active LLM call options".to_string(),
                    )
                })?,
            )
            .await?
        }
        CompactStrategy::Custom => {
            custom_compaction_summary(
                &old_messages,
                archived_count,
                config.custom_compactor.as_ref().ok_or_else(|| {
                    VmError::Runtime(
                        "compact_callback is required when compact_strategy is 'custom'"
                            .to_string(),
                    )
                })?,
            )
            .await?
        }
    };
    messages.insert(
        0,
        serde_json::json!({
            "role": "user",
            "content": summary,
        }),
    );
    Ok(Some(summary))
}

// ── Adaptive context assembly ─────────────────────────────────────────

/// Snip an artifact's text to fit within a token budget.
pub fn microcompact_artifact(artifact: &mut ArtifactRecord, max_tokens: usize) {
    let max_chars = max_tokens * 4;
    if let Some(ref text) = artifact.text {
        if text.len() > max_chars && max_chars >= 200 {
            artifact.text = Some(microcompact_tool_output(text, max_chars));
            artifact.estimated_tokens = Some(max_tokens);
        }
    }
}

/// Deduplicate artifacts by removing those with identical text content,
/// keeping the one with higher priority.
pub fn dedup_artifacts(artifacts: &mut Vec<ArtifactRecord>) {
    let mut seen_hashes: BTreeSet<u64> = BTreeSet::new();
    artifacts.retain(|artifact| {
        let text = artifact.text.as_deref().unwrap_or("");
        if text.is_empty() {
            return true;
        }
        // Simple hash for dedup
        let hash = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            text.hash(&mut hasher);
            hasher.finish()
        };
        seen_hashes.insert(hash)
    });
}

/// Enhanced artifact selection: dedup, microcompact oversized artifacts,
/// then delegate to the standard `select_artifacts`.
pub fn select_artifacts_adaptive(
    mut artifacts: Vec<ArtifactRecord>,
    policy: &ContextPolicy,
) -> Vec<ArtifactRecord> {
    // Phase 1: deduplicate
    dedup_artifacts(&mut artifacts);

    // Phase 2: microcompact oversized artifacts relative to budget.
    // Cap individual artifacts to a fraction of the total budget, but don't
    // let the per-artifact cap exceed the total budget (avoid overrun).
    if let Some(max_tokens) = policy.max_tokens {
        let count = artifacts.len().max(1);
        let per_artifact_budget = max_tokens / count;
        // Floor of 500 tokens, but never more than total budget
        let cap = per_artifact_budget.max(500).min(max_tokens);
        for artifact in &mut artifacts {
            let est = artifact.estimated_tokens.unwrap_or(0);
            if est > cap * 2 {
                microcompact_artifact(artifact, cap);
            }
        }
    }

    // Phase 3: standard selection with budget
    select_artifacts(artifacts, policy)
}

// ── Per-agent policy with argument patterns ───────────────────────────

/// Extended policy that supports argument-level constraints.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ToolArgConstraint {
    /// Tool name to constrain.
    pub tool: String,
    /// Glob patterns that the first string argument must match.
    /// If empty, no argument constraint is applied.
    pub arg_patterns: Vec<String>,
}

/// Check if a tool call satisfies argument constraints in the policy.
pub fn enforce_tool_arg_constraints(
    policy: &CapabilityPolicy,
    tool_name: &str,
    args: &serde_json::Value,
) -> Result<(), VmError> {
    for constraint in &policy.tool_arg_constraints {
        if !glob_match(&constraint.tool, tool_name) {
            continue;
        }
        if constraint.arg_patterns.is_empty() {
            continue;
        }
        // Extract the first string-like argument for pattern matching
        let first_arg = args
            .as_object()
            .and_then(|o| o.values().next())
            .and_then(|v| v.as_str())
            .or_else(|| args.as_str())
            .unwrap_or("");
        let matches = constraint
            .arg_patterns
            .iter()
            .any(|pattern| glob_match(pattern, first_arg));
        if !matches {
            return reject_policy(format!(
                "tool '{tool_name}' argument '{first_arg}' does not match allowed patterns: {:?}",
                constraint.arg_patterns
            ));
        }
    }
    Ok(())
}

fn normalize_artifact_kind(kind: &str) -> String {
    match kind {
        "resource"
        | "workspace_file"
        | "editor_selection"
        | "workspace_snapshot"
        | "transcript_summary"
        | "summary"
        | "plan"
        | "diff"
        | "git_diff"
        | "patch"
        | "patch_set"
        | "patch_proposal"
        | "diff_review"
        | "review_decision"
        | "verification_bundle"
        | "apply_intent"
        | "verification_result"
        | "test_result"
        | "command_result"
        | "provider_payload"
        | "worker_result"
        | "worker_notification"
        | "artifact" => kind.to_string(),
        "file" => "workspace_file".to_string(),
        "transcript" => "transcript_summary".to_string(),
        "verification" => "verification_result".to_string(),
        "test" => "test_result".to_string(),
        other if other.trim().is_empty() => "artifact".to_string(),
        other => other.to_string(),
    }
}

fn default_artifact_priority(kind: &str) -> i64 {
    match kind {
        "verification_result" | "test_result" => 100,
        "verification_bundle" => 95,
        "diff" | "git_diff" | "patch" | "patch_set" | "patch_proposal" | "diff_review"
        | "review_decision" | "apply_intent" => 90,
        "plan" => 80,
        "workspace_file" | "workspace_snapshot" | "editor_selection" | "resource" => 70,
        "summary" | "transcript_summary" => 60,
        "command_result" => 50,
        _ => 40,
    }
}

fn freshness_rank(value: Option<&str>) -> i64 {
    match value.unwrap_or_default() {
        "fresh" | "live" => 3,
        "recent" => 2,
        "stale" => 0,
        _ => 1,
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ToolRuntimePolicyMetadata {
    pub capabilities: BTreeMap<String, Vec<String>>,
    pub side_effect_level: Option<String>,
    pub path_params: Vec<String>,
    pub mutation_classification: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CapabilityPolicy {
    pub tools: Vec<String>,
    pub capabilities: BTreeMap<String, Vec<String>>,
    pub workspace_roots: Vec<String>,
    pub side_effect_level: Option<String>,
    pub recursion_limit: Option<usize>,
    /// Argument-level constraints for specific tools.
    #[serde(default)]
    pub tool_arg_constraints: Vec<ToolArgConstraint>,
    #[serde(default)]
    pub tool_metadata: BTreeMap<String, ToolRuntimePolicyMetadata>,
}

impl CapabilityPolicy {
    pub fn intersect(&self, requested: &CapabilityPolicy) -> Result<CapabilityPolicy, String> {
        let side_effect_level = match (&self.side_effect_level, &requested.side_effect_level) {
            (Some(a), Some(b)) => Some(min_side_effect(a, b).to_string()),
            (Some(a), None) => Some(a.clone()),
            (None, Some(b)) => Some(b.clone()),
            (None, None) => None,
        };

        if !self.tools.is_empty() {
            let denied: Vec<String> = requested
                .tools
                .iter()
                .filter(|tool| !self.tools.contains(*tool))
                .cloned()
                .collect();
            if !denied.is_empty() {
                return Err(format!(
                    "requested tools exceed host ceiling: {}",
                    denied.join(", ")
                ));
            }
        }

        for (capability, requested_ops) in &requested.capabilities {
            if let Some(allowed_ops) = self.capabilities.get(capability) {
                let denied: Vec<String> = requested_ops
                    .iter()
                    .filter(|op| !allowed_ops.contains(*op))
                    .cloned()
                    .collect();
                if !denied.is_empty() {
                    return Err(format!(
                        "requested capability operations exceed host ceiling: {}.{}",
                        capability,
                        denied.join(",")
                    ));
                }
            } else if !self.capabilities.is_empty() {
                return Err(format!(
                    "requested capability exceeds host ceiling: {capability}"
                ));
            }
        }

        let tools = if self.tools.is_empty() {
            requested.tools.clone()
        } else if requested.tools.is_empty() {
            self.tools.clone()
        } else {
            requested
                .tools
                .iter()
                .filter(|tool| self.tools.contains(*tool))
                .cloned()
                .collect()
        };

        let capabilities = if self.capabilities.is_empty() {
            requested.capabilities.clone()
        } else if requested.capabilities.is_empty() {
            self.capabilities.clone()
        } else {
            requested
                .capabilities
                .iter()
                .filter_map(|(capability, requested_ops)| {
                    self.capabilities.get(capability).map(|allowed_ops| {
                        (
                            capability.clone(),
                            requested_ops
                                .iter()
                                .filter(|op| allowed_ops.contains(*op))
                                .cloned()
                                .collect::<Vec<_>>(),
                        )
                    })
                })
                .collect()
        };

        let workspace_roots = if self.workspace_roots.is_empty() {
            requested.workspace_roots.clone()
        } else if requested.workspace_roots.is_empty() {
            self.workspace_roots.clone()
        } else {
            requested
                .workspace_roots
                .iter()
                .filter(|root| self.workspace_roots.contains(*root))
                .cloned()
                .collect()
        };

        let recursion_limit = match (self.recursion_limit, requested.recursion_limit) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };

        // Merge arg constraints from both sides
        let mut tool_arg_constraints = self.tool_arg_constraints.clone();
        tool_arg_constraints.extend(requested.tool_arg_constraints.clone());

        let tool_metadata = tools
            .iter()
            .filter_map(|tool| {
                requested
                    .tool_metadata
                    .get(tool)
                    .or_else(|| self.tool_metadata.get(tool))
                    .cloned()
                    .map(|metadata| (tool.clone(), metadata))
            })
            .collect();

        Ok(CapabilityPolicy {
            tools,
            capabilities,
            workspace_roots,
            side_effect_level,
            recursion_limit,
            tool_arg_constraints,
            tool_metadata,
        })
    }
}

fn min_side_effect<'a>(a: &'a str, b: &'a str) -> &'a str {
    fn rank(v: &str) -> usize {
        match v {
            "none" => 0,
            "read_only" => 1,
            "workspace_write" => 2,
            "process_exec" => 3,
            "network" => 4,
            _ => 5,
        }
    }
    if rank(a) <= rank(b) {
        a
    } else {
        b
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ModelPolicy {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub model_tier: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TranscriptPolicy {
    pub mode: Option<String>,
    pub visibility: Option<String>,
    pub summarize: bool,
    pub compact: bool,
    pub keep_last: Option<usize>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextPolicy {
    pub max_artifacts: Option<usize>,
    pub max_tokens: Option<usize>,
    pub reserve_tokens: Option<usize>,
    pub include_kinds: Vec<String>,
    pub exclude_kinds: Vec<String>,
    pub prioritize_kinds: Vec<String>,
    pub pinned_ids: Vec<String>,
    pub include_stages: Vec<String>,
    pub prefer_recent: bool,
    pub prefer_fresh: bool,
    pub render: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RetryPolicy {
    pub max_attempts: usize,
    pub verify: bool,
    pub repair: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StageContract {
    pub input_kinds: Vec<String>,
    pub output_kinds: Vec<String>,
    pub min_inputs: Option<usize>,
    pub max_inputs: Option<usize>,
    pub require_transcript: bool,
    pub schema: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BranchSemantics {
    pub success: Option<String>,
    pub failure: Option<String>,
    pub verify_pass: Option<String>,
    pub verify_fail: Option<String>,
    pub condition_true: Option<String>,
    pub condition_false: Option<String>,
    pub loop_continue: Option<String>,
    pub loop_exit: Option<String>,
    pub escalation: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MapPolicy {
    pub items: Vec<serde_json::Value>,
    pub item_artifact_kind: Option<String>,
    pub output_kind: Option<String>,
    pub max_items: Option<usize>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct JoinPolicy {
    pub strategy: String,
    pub require_all_inputs: bool,
    pub min_completed: Option<usize>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReducePolicy {
    pub strategy: String,
    pub separator: Option<String>,
    pub output_kind: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EscalationPolicy {
    pub level: Option<String>,
    pub queue: Option<String>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ArtifactRecord {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub id: String,
    pub kind: String,
    pub title: Option<String>,
    pub text: Option<String>,
    pub data: Option<serde_json::Value>,
    pub source: Option<String>,
    pub created_at: String,
    pub freshness: Option<String>,
    pub priority: Option<i64>,
    pub lineage: Vec<String>,
    pub relevance: Option<f64>,
    pub estimated_tokens: Option<usize>,
    pub stage: Option<String>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl ArtifactRecord {
    pub fn normalize(mut self) -> Self {
        if self.type_name.is_empty() {
            self.type_name = "artifact".to_string();
        }
        if self.id.is_empty() {
            self.id = new_id("artifact");
        }
        if self.created_at.is_empty() {
            self.created_at = now_rfc3339();
        }
        if self.kind.is_empty() {
            self.kind = "artifact".to_string();
        }
        self.kind = normalize_artifact_kind(&self.kind);
        if self.estimated_tokens.is_none() {
            self.estimated_tokens = self
                .text
                .as_ref()
                .map(|text| ((text.len() as f64) / 4.0).ceil() as usize);
        }
        if self.priority.is_none() {
            self.priority = Some(default_artifact_priority(&self.kind));
        }
        self
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkflowNode {
    pub id: Option<String>,
    pub kind: String,
    pub mode: Option<String>,
    pub prompt: Option<String>,
    pub system: Option<String>,
    pub task_label: Option<String>,
    pub done_sentinel: Option<String>,
    pub tools: serde_json::Value,
    pub model_policy: ModelPolicy,
    pub transcript_policy: TranscriptPolicy,
    pub context_policy: ContextPolicy,
    pub retry_policy: RetryPolicy,
    pub capability_policy: CapabilityPolicy,
    pub input_contract: StageContract,
    pub output_contract: StageContract,
    pub branch_semantics: BranchSemantics,
    pub map_policy: MapPolicy,
    pub join_policy: JoinPolicy,
    pub reduce_policy: ReducePolicy,
    pub escalation_policy: EscalationPolicy,
    pub verify: Option<serde_json::Value>,
    pub metadata: BTreeMap<String, serde_json::Value>,
    #[serde(skip)]
    pub raw_tools: Option<VmValue>,
}

impl PartialEq for WorkflowNode {
    fn eq(&self, other: &Self) -> bool {
        serde_json::to_value(self).ok() == serde_json::to_value(other).ok()
    }
}

pub fn workflow_tool_names(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Null => Vec::new(),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                serde_json::Value::Object(map) => map
                    .get("name")
                    .and_then(|value| value.as_str())
                    .filter(|name| !name.is_empty())
                    .map(|name| name.to_string()),
                _ => None,
            })
            .collect(),
        serde_json::Value::Object(map) => {
            if map.get("_type").and_then(|value| value.as_str()) == Some("tool_registry") {
                return map
                    .get("tools")
                    .map(workflow_tool_names)
                    .unwrap_or_default();
            }
            map.get("name")
                .and_then(|value| value.as_str())
                .filter(|name| !name.is_empty())
                .map(|name| vec![name.to_string()])
                .unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

fn max_side_effect_level(levels: impl Iterator<Item = String>) -> Option<String> {
    fn rank(v: &str) -> usize {
        match v {
            "none" => 0,
            "read_only" => 1,
            "workspace_write" => 2,
            "process_exec" => 3,
            "network" => 4,
            _ => 5,
        }
    }
    levels.max_by_key(|level| rank(level))
}

fn parse_tool_runtime_policy(
    map: &serde_json::Map<String, serde_json::Value>,
) -> ToolRuntimePolicyMetadata {
    let Some(policy) = map.get("policy").and_then(|value| value.as_object()) else {
        return ToolRuntimePolicyMetadata::default();
    };

    let capabilities = policy
        .get("capabilities")
        .and_then(|value| value.as_object())
        .map(|caps| {
            caps.iter()
                .map(|(capability, ops)| {
                    let values = ops
                        .as_array()
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|item| item.as_str().map(|s| s.to_string()))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    (capability.clone(), values)
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();

    let path_params = policy
        .get("path_params")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    ToolRuntimePolicyMetadata {
        capabilities,
        side_effect_level: policy
            .get("side_effect_level")
            .and_then(|value| value.as_str())
            .map(|s| s.to_string()),
        path_params,
        mutation_classification: policy
            .get("mutation_classification")
            .and_then(|value| value.as_str())
            .map(|s| s.to_string()),
    }
}

pub fn workflow_tool_metadata(
    value: &serde_json::Value,
) -> BTreeMap<String, ToolRuntimePolicyMetadata> {
    match value {
        serde_json::Value::Null => BTreeMap::new(),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                serde_json::Value::Object(map) => map
                    .get("name")
                    .and_then(|value| value.as_str())
                    .filter(|name| !name.is_empty())
                    .map(|name| (name.to_string(), parse_tool_runtime_policy(map))),
                _ => None,
            })
            .collect(),
        serde_json::Value::Object(map) => {
            if map.get("_type").and_then(|value| value.as_str()) == Some("tool_registry") {
                return map
                    .get("tools")
                    .map(workflow_tool_metadata)
                    .unwrap_or_default();
            }
            map.get("name")
                .and_then(|value| value.as_str())
                .filter(|name| !name.is_empty())
                .map(|name| {
                    let mut metadata = BTreeMap::new();
                    metadata.insert(name.to_string(), parse_tool_runtime_policy(map));
                    metadata
                })
                .unwrap_or_default()
        }
        _ => BTreeMap::new(),
    }
}

pub fn workflow_tool_policy_from_tools(value: &serde_json::Value) -> CapabilityPolicy {
    let tools = workflow_tool_names(value);
    let tool_metadata = workflow_tool_metadata(value);
    let mut capabilities: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for metadata in tool_metadata.values() {
        for (capability, ops) in &metadata.capabilities {
            let entry = capabilities.entry(capability.clone()).or_default();
            for op in ops {
                if !entry.contains(op) {
                    entry.push(op.clone());
                }
            }
            entry.sort();
        }
    }
    let side_effect_level = max_side_effect_level(
        tool_metadata
            .values()
            .filter_map(|metadata| metadata.side_effect_level.clone()),
    );
    CapabilityPolicy {
        tools,
        capabilities,
        workspace_roots: Vec::new(),
        side_effect_level,
        recursion_limit: None,
        tool_arg_constraints: Vec::new(),
        tool_metadata,
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorkflowEdge {
    pub from: String,
    pub to: String,
    pub branch: Option<String>,
    pub label: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WorkflowGraph {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub id: String,
    pub name: Option<String>,
    pub version: usize,
    pub entry: String,
    pub nodes: BTreeMap<String, WorkflowNode>,
    pub edges: Vec<WorkflowEdge>,
    pub capability_policy: CapabilityPolicy,
    pub metadata: BTreeMap<String, serde_json::Value>,
    pub audit_log: Vec<WorkflowAuditEntry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WorkflowAuditEntry {
    pub id: String,
    pub op: String,
    pub node_id: Option<String>,
    pub timestamp: String,
    pub reason: Option<String>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LlmUsageRecord {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_duration_ms: i64,
    pub call_count: i64,
    pub total_cost: f64,
    pub models: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RunStageRecord {
    pub id: String,
    pub node_id: String,
    pub kind: String,
    pub status: String,
    pub outcome: String,
    pub branch: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub visible_text: Option<String>,
    pub private_reasoning: Option<String>,
    pub transcript: Option<serde_json::Value>,
    pub verification: Option<serde_json::Value>,
    pub usage: Option<LlmUsageRecord>,
    pub artifacts: Vec<ArtifactRecord>,
    pub consumed_artifact_ids: Vec<String>,
    pub produced_artifact_ids: Vec<String>,
    pub attempts: Vec<RunStageAttemptRecord>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RunStageAttemptRecord {
    pub attempt: usize,
    pub status: String,
    pub outcome: String,
    pub branch: Option<String>,
    pub error: Option<String>,
    pub verification: Option<serde_json::Value>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunTransitionRecord {
    pub id: String,
    pub from_stage_id: Option<String>,
    pub from_node_id: Option<String>,
    pub to_node_id: String,
    pub branch: Option<String>,
    pub timestamp: String,
    pub consumed_artifact_ids: Vec<String>,
    pub produced_artifact_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunCheckpointRecord {
    pub id: String,
    pub ready_nodes: Vec<String>,
    pub completed_nodes: Vec<String>,
    pub last_stage_id: Option<String>,
    pub persisted_at: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReplayFixture {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub id: String,
    pub source_run_id: String,
    pub workflow_id: String,
    pub workflow_name: Option<String>,
    pub created_at: String,
    pub expected_status: String,
    pub stage_assertions: Vec<ReplayStageAssertion>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReplayStageAssertion {
    pub node_id: String,
    pub expected_status: String,
    pub expected_outcome: String,
    pub expected_branch: Option<String>,
    pub required_artifact_kinds: Vec<String>,
    pub visible_text_contains: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReplayEvalReport {
    pub pass: bool,
    pub failures: Vec<String>,
    pub stage_count: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReplayEvalCaseReport {
    pub run_id: String,
    pub workflow_id: String,
    pub label: Option<String>,
    pub pass: bool,
    pub failures: Vec<String>,
    pub stage_count: usize,
    pub source_path: Option<String>,
    pub comparison: Option<RunDiffReport>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReplayEvalSuiteReport {
    pub pass: bool,
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub cases: Vec<ReplayEvalCaseReport>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunStageDiffRecord {
    pub node_id: String,
    pub change: String,
    pub details: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunDiffReport {
    pub left_run_id: String,
    pub right_run_id: String,
    pub identical: bool,
    pub status_changed: bool,
    pub left_status: String,
    pub right_status: String,
    pub stage_diffs: Vec<RunStageDiffRecord>,
    pub transition_count_delta: isize,
    pub artifact_count_delta: isize,
    pub checkpoint_count_delta: isize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EvalSuiteManifest {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub id: String,
    pub name: Option<String>,
    pub base_dir: Option<String>,
    pub cases: Vec<EvalSuiteCase>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EvalSuiteCase {
    pub label: Option<String>,
    pub run_path: String,
    pub fixture_path: Option<String>,
    pub compare_to: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RunRecord {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub id: String,
    pub workflow_id: String,
    pub workflow_name: Option<String>,
    pub task: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub parent_run_id: Option<String>,
    pub root_run_id: Option<String>,
    pub stages: Vec<RunStageRecord>,
    pub transitions: Vec<RunTransitionRecord>,
    pub checkpoints: Vec<RunCheckpointRecord>,
    pub pending_nodes: Vec<String>,
    pub completed_nodes: Vec<String>,
    pub child_runs: Vec<RunChildRecord>,
    pub artifacts: Vec<ArtifactRecord>,
    pub policy: CapabilityPolicy,
    pub execution: Option<RunExecutionRecord>,
    pub transcript: Option<serde_json::Value>,
    pub usage: Option<LlmUsageRecord>,
    pub replay_fixture: Option<ReplayFixture>,
    pub trace_spans: Vec<RunTraceSpanRecord>,
    pub metadata: BTreeMap<String, serde_json::Value>,
    pub persisted_path: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunTraceSpanRecord {
    pub span_id: u64,
    pub parent_id: Option<u64>,
    pub kind: String,
    pub name: String,
    pub start_ms: u64,
    pub duration_ms: u64,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunChildRecord {
    pub worker_id: String,
    pub worker_name: String,
    pub parent_stage_id: Option<String>,
    pub session_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub mutation_scope: Option<String>,
    pub approval_mode: Option<String>,
    pub task: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub run_id: Option<String>,
    pub run_path: Option<String>,
    pub snapshot_path: Option<String>,
    pub execution: Option<RunExecutionRecord>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RunExecutionRecord {
    pub cwd: Option<String>,
    pub source_dir: Option<String>,
    pub env: BTreeMap<String, String>,
    pub adapter: Option<String>,
    pub repo_path: Option<String>,
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
    pub base_ref: Option<String>,
    pub cleanup: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorkflowValidationReport {
    pub valid: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub reachable_nodes: Vec<String>,
}

fn parse_json_payload<T: for<'de> Deserialize<'de>>(
    json: serde_json::Value,
    label: &str,
) -> Result<T, VmError> {
    let payload = json.to_string();
    let mut deserializer = serde_json::Deserializer::from_str(&payload);
    let mut tracker = serde_path_to_error::Track::new();
    let path_deserializer = serde_path_to_error::Deserializer::new(&mut deserializer, &mut tracker);
    T::deserialize(path_deserializer).map_err(|error| {
        let snippet = if payload.len() > 600 {
            format!("{}...", &payload[..600])
        } else {
            payload.clone()
        };
        VmError::Runtime(format!(
            "{label} parse error at {}: {} | payload={}",
            tracker.path(),
            error,
            snippet
        ))
    })
}

fn parse_json_value<T: for<'de> Deserialize<'de>>(value: &VmValue) -> Result<T, VmError> {
    parse_json_payload(vm_value_to_json(value), "orchestration")
}

pub fn parse_workflow_node_value(value: &VmValue, label: &str) -> Result<WorkflowNode, VmError> {
    let mut node: WorkflowNode = parse_json_payload(vm_value_to_json(value), label)?;
    node.raw_tools = value.as_dict().and_then(|dict| dict.get("tools")).cloned();
    Ok(node)
}

pub fn parse_workflow_node_json(
    json: serde_json::Value,
    label: &str,
) -> Result<WorkflowNode, VmError> {
    parse_json_payload(json, label)
}

pub fn parse_workflow_edge_json(
    json: serde_json::Value,
    label: &str,
) -> Result<WorkflowEdge, VmError> {
    parse_json_payload(json, label)
}

pub fn normalize_workflow_value(value: &VmValue) -> Result<WorkflowGraph, VmError> {
    let mut graph: WorkflowGraph = parse_json_value(value)?;
    let as_dict = value.as_dict().cloned().unwrap_or_default();

    if graph.nodes.is_empty() {
        for key in ["act", "verify", "repair"] {
            if let Some(node_value) = as_dict.get(key) {
                let mut node = parse_workflow_node_value(node_value, "orchestration")?;
                let raw_node = node_value.as_dict().cloned().unwrap_or_default();
                node.id = Some(key.to_string());
                if node.kind.is_empty() {
                    node.kind = if key == "verify" {
                        "verify".to_string()
                    } else {
                        "stage".to_string()
                    };
                }
                if node.model_policy.provider.is_none() {
                    node.model_policy.provider = as_dict
                        .get("provider")
                        .map(|value| value.display())
                        .filter(|value| !value.is_empty());
                }
                if node.model_policy.model.is_none() {
                    node.model_policy.model = as_dict
                        .get("model")
                        .map(|value| value.display())
                        .filter(|value| !value.is_empty());
                }
                if node.model_policy.model_tier.is_none() {
                    node.model_policy.model_tier = as_dict
                        .get("model_tier")
                        .or_else(|| as_dict.get("tier"))
                        .map(|value| value.display())
                        .filter(|value| !value.is_empty());
                }
                if node.model_policy.temperature.is_none() {
                    node.model_policy.temperature = as_dict.get("temperature").and_then(|value| {
                        if let VmValue::Float(number) = value {
                            Some(*number)
                        } else {
                            value.as_int().map(|number| number as f64)
                        }
                    });
                }
                if node.model_policy.max_tokens.is_none() {
                    node.model_policy.max_tokens =
                        as_dict.get("max_tokens").and_then(|value| value.as_int());
                }
                if node.mode.is_none() {
                    node.mode = as_dict
                        .get("mode")
                        .map(|value| value.display())
                        .filter(|value| !value.is_empty());
                }
                if node.done_sentinel.is_none() {
                    node.done_sentinel = as_dict
                        .get("done_sentinel")
                        .map(|value| value.display())
                        .filter(|value| !value.is_empty());
                }
                if key == "verify"
                    && node.verify.is_none()
                    && (raw_node.contains_key("assert_text")
                        || raw_node.contains_key("command")
                        || raw_node.contains_key("expect_status")
                        || raw_node.contains_key("expect_text"))
                {
                    node.verify = Some(serde_json::json!({
                        "assert_text": raw_node.get("assert_text").map(vm_value_to_json),
                        "command": raw_node.get("command").map(vm_value_to_json),
                        "expect_status": raw_node.get("expect_status").map(vm_value_to_json),
                        "expect_text": raw_node.get("expect_text").map(vm_value_to_json),
                    }));
                }
                graph.nodes.insert(key.to_string(), node);
            }
        }
        if graph.entry.is_empty() && graph.nodes.contains_key("act") {
            graph.entry = "act".to_string();
        }
        if graph.edges.is_empty() && graph.nodes.contains_key("act") {
            if graph.nodes.contains_key("verify") {
                graph.edges.push(WorkflowEdge {
                    from: "act".to_string(),
                    to: "verify".to_string(),
                    branch: None,
                    label: None,
                });
            }
            if graph.nodes.contains_key("repair") {
                graph.edges.push(WorkflowEdge {
                    from: "verify".to_string(),
                    to: "repair".to_string(),
                    branch: Some("failed".to_string()),
                    label: None,
                });
                graph.edges.push(WorkflowEdge {
                    from: "repair".to_string(),
                    to: "verify".to_string(),
                    branch: Some("retry".to_string()),
                    label: None,
                });
            }
        }
    }

    if graph.type_name.is_empty() {
        graph.type_name = "workflow_graph".to_string();
    }
    if graph.id.is_empty() {
        graph.id = new_id("workflow");
    }
    if graph.version == 0 {
        graph.version = 1;
    }
    if graph.entry.is_empty() {
        graph.entry = graph
            .nodes
            .keys()
            .next()
            .cloned()
            .unwrap_or_else(|| "act".to_string());
    }
    for (node_id, node) in &mut graph.nodes {
        if node.raw_tools.is_none() {
            node.raw_tools = as_dict
                .get("nodes")
                .and_then(|nodes| nodes.as_dict())
                .and_then(|nodes| nodes.get(node_id))
                .and_then(|node_value| node_value.as_dict())
                .and_then(|raw_node| raw_node.get("tools"))
                .cloned();
        }
        if node.id.is_none() {
            node.id = Some(node_id.clone());
        }
        if node.kind.is_empty() {
            node.kind = "stage".to_string();
        }
        if node.join_policy.strategy.is_empty() {
            node.join_policy.strategy = "all".to_string();
        }
        if node.reduce_policy.strategy.is_empty() {
            node.reduce_policy.strategy = "concat".to_string();
        }
        if node.output_contract.output_kinds.is_empty() {
            node.output_contract.output_kinds = vec![match node.kind.as_str() {
                "verify" => "verification_result".to_string(),
                "reduce" => node
                    .reduce_policy
                    .output_kind
                    .clone()
                    .unwrap_or_else(|| "summary".to_string()),
                "map" => node
                    .map_policy
                    .output_kind
                    .clone()
                    .unwrap_or_else(|| "artifact".to_string()),
                "escalation" => "plan".to_string(),
                _ => "artifact".to_string(),
            }];
        }
        if node.retry_policy.max_attempts == 0 {
            node.retry_policy.max_attempts = 1;
        }
    }
    Ok(graph)
}

pub fn validate_workflow(
    graph: &WorkflowGraph,
    ceiling: Option<&CapabilityPolicy>,
) -> WorkflowValidationReport {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    if !graph.nodes.contains_key(&graph.entry) {
        errors.push(format!("entry node does not exist: {}", graph.entry));
    }

    let node_ids: BTreeSet<String> = graph.nodes.keys().cloned().collect();
    for edge in &graph.edges {
        if !node_ids.contains(&edge.from) {
            errors.push(format!("edge.from references unknown node: {}", edge.from));
        }
        if !node_ids.contains(&edge.to) {
            errors.push(format!("edge.to references unknown node: {}", edge.to));
        }
    }

    let reachable_nodes = reachable_nodes(graph);
    for node_id in &node_ids {
        if !reachable_nodes.contains(node_id) {
            warnings.push(format!("node is unreachable: {node_id}"));
        }
    }

    for (node_id, node) in &graph.nodes {
        let incoming = graph
            .edges
            .iter()
            .filter(|edge| edge.to == *node_id)
            .count();
        let outgoing: Vec<&WorkflowEdge> = graph
            .edges
            .iter()
            .filter(|edge| edge.from == *node_id)
            .collect();
        if let Some(min_inputs) = node.input_contract.min_inputs {
            if let Some(max_inputs) = node.input_contract.max_inputs {
                if min_inputs > max_inputs {
                    errors.push(format!(
                        "node {node_id}: input contract min_inputs exceeds max_inputs"
                    ));
                }
            }
        }
        match node.kind.as_str() {
            "condition" => {
                let has_true = outgoing
                    .iter()
                    .any(|edge| edge.branch.as_deref() == Some("true"));
                let has_false = outgoing
                    .iter()
                    .any(|edge| edge.branch.as_deref() == Some("false"));
                if !has_true || !has_false {
                    errors.push(format!(
                        "node {node_id}: condition nodes require both 'true' and 'false' branch edges"
                    ));
                }
            }
            "fork" => {
                if outgoing.len() < 2 {
                    errors.push(format!(
                        "node {node_id}: fork nodes require at least two outgoing edges"
                    ));
                }
            }
            "join" => {
                if incoming < 2 {
                    warnings.push(format!(
                        "node {node_id}: join node has fewer than two incoming edges"
                    ));
                }
            }
            "map" => {
                if node.map_policy.items.is_empty()
                    && node.map_policy.item_artifact_kind.is_none()
                    && node.input_contract.input_kinds.is_empty()
                {
                    errors.push(format!(
                        "node {node_id}: map nodes require items, item_artifact_kind, or input_contract.input_kinds"
                    ));
                }
            }
            "reduce" => {
                if node.input_contract.input_kinds.is_empty() {
                    warnings.push(format!(
                        "node {node_id}: reduce node has no input_contract.input_kinds; it will consume all available artifacts"
                    ));
                }
            }
            _ => {}
        }
    }

    if let Some(ceiling) = ceiling {
        if let Err(error) = ceiling.intersect(&graph.capability_policy) {
            errors.push(error);
        }
        for (node_id, node) in &graph.nodes {
            if let Err(error) = ceiling.intersect(&node.capability_policy) {
                errors.push(format!("node {node_id}: {error}"));
            }
        }
    }

    WorkflowValidationReport {
        valid: errors.is_empty(),
        errors,
        warnings,
        reachable_nodes: reachable_nodes.into_iter().collect(),
    }
}

fn reachable_nodes(graph: &WorkflowGraph) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![graph.entry.clone()];
    while let Some(node_id) = stack.pop() {
        if !seen.insert(node_id.clone()) {
            continue;
        }
        for edge in graph.edges.iter().filter(|edge| edge.from == node_id) {
            stack.push(edge.to.clone());
        }
    }
    seen
}

pub fn select_artifacts(
    mut artifacts: Vec<ArtifactRecord>,
    policy: &ContextPolicy,
) -> Vec<ArtifactRecord> {
    artifacts.retain(|artifact| {
        (policy.include_kinds.is_empty() || policy.include_kinds.contains(&artifact.kind))
            && !policy.exclude_kinds.contains(&artifact.kind)
            && (policy.include_stages.is_empty()
                || artifact
                    .stage
                    .as_ref()
                    .is_some_and(|stage| policy.include_stages.contains(stage)))
    });
    artifacts.sort_by(|a, b| {
        let b_pinned = policy.pinned_ids.contains(&b.id);
        let a_pinned = policy.pinned_ids.contains(&a.id);
        b_pinned
            .cmp(&a_pinned)
            .then_with(|| {
                let b_prio_kind = policy.prioritize_kinds.contains(&b.kind);
                let a_prio_kind = policy.prioritize_kinds.contains(&a.kind);
                b_prio_kind.cmp(&a_prio_kind)
            })
            .then_with(|| {
                b.priority
                    .unwrap_or_default()
                    .cmp(&a.priority.unwrap_or_default())
            })
            .then_with(|| {
                if policy.prefer_fresh {
                    freshness_rank(b.freshness.as_deref())
                        .cmp(&freshness_rank(a.freshness.as_deref()))
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .then_with(|| {
                if policy.prefer_recent {
                    b.created_at.cmp(&a.created_at)
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .then_with(|| {
                b.relevance
                    .partial_cmp(&a.relevance)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                a.estimated_tokens
                    .unwrap_or(usize::MAX)
                    .cmp(&b.estimated_tokens.unwrap_or(usize::MAX))
            })
    });

    let mut selected = Vec::new();
    let mut used_tokens = 0usize;
    let reserve_tokens = policy.reserve_tokens.unwrap_or(0);
    let effective_max_tokens = policy
        .max_tokens
        .map(|max| max.saturating_sub(reserve_tokens));
    for artifact in artifacts {
        if let Some(max_artifacts) = policy.max_artifacts {
            if selected.len() >= max_artifacts {
                break;
            }
        }
        let next_tokens = artifact.estimated_tokens.unwrap_or(0);
        if let Some(max_tokens) = effective_max_tokens {
            if used_tokens + next_tokens > max_tokens {
                continue;
            }
        }
        used_tokens += next_tokens;
        selected.push(artifact);
    }
    selected
}

pub fn render_artifacts_context(artifacts: &[ArtifactRecord], policy: &ContextPolicy) -> String {
    let mut parts = Vec::new();
    for artifact in artifacts {
        let title = artifact
            .title
            .clone()
            .unwrap_or_else(|| format!("{} {}", artifact.kind, artifact.id));
        let body = artifact
            .text
            .clone()
            .or_else(|| artifact.data.as_ref().map(|v| v.to_string()))
            .unwrap_or_default();
        match policy.render.as_deref() {
            Some("json") => {
                parts.push(
                    serde_json::json!({
                        "id": artifact.id,
                        "kind": artifact.kind,
                        "title": title,
                        "source": artifact.source,
                        "freshness": artifact.freshness,
                        "priority": artifact.priority,
                        "text": body,
                    })
                    .to_string(),
                );
            }
            _ => parts.push(format!(
                "[{title}] kind={} source={} freshness={} priority={}\n{}",
                artifact.kind,
                artifact
                    .source
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                artifact
                    .freshness
                    .clone()
                    .unwrap_or_else(|| "normal".to_string()),
                artifact.priority.unwrap_or_default(),
                body
            )),
        }
    }
    parts.join("\n\n")
}

pub fn normalize_artifact(value: &VmValue) -> Result<ArtifactRecord, VmError> {
    let artifact: ArtifactRecord = parse_json_value(value)?;
    Ok(artifact.normalize())
}

pub fn normalize_run_record(value: &VmValue) -> Result<RunRecord, VmError> {
    let json = vm_value_to_json(value);
    let payload = json.to_string();
    let mut deserializer = serde_json::Deserializer::from_str(&payload);
    let mut tracker = serde_path_to_error::Track::new();
    let path_deserializer = serde_path_to_error::Deserializer::new(&mut deserializer, &mut tracker);
    let mut run: RunRecord = RunRecord::deserialize(path_deserializer).map_err(|error| {
        let snippet = if payload.len() > 600 {
            format!("{}...", &payload[..600])
        } else {
            payload.clone()
        };
        VmError::Runtime(format!(
            "orchestration parse error at {}: {} | payload={}",
            tracker.path(),
            error,
            snippet
        ))
    })?;
    if run.type_name.is_empty() {
        run.type_name = "run_record".to_string();
    }
    if run.id.is_empty() {
        run.id = new_id("run");
    }
    if run.started_at.is_empty() {
        run.started_at = now_rfc3339();
    }
    if run.status.is_empty() {
        run.status = "running".to_string();
    }
    if run.root_run_id.is_none() {
        run.root_run_id = Some(run.id.clone());
    }
    if run.replay_fixture.is_none() {
        run.replay_fixture = Some(replay_fixture_from_run(&run));
    }
    Ok(run)
}

pub fn normalize_eval_suite_manifest(value: &VmValue) -> Result<EvalSuiteManifest, VmError> {
    let mut manifest: EvalSuiteManifest = parse_json_value(value)?;
    if manifest.type_name.is_empty() {
        manifest.type_name = "eval_suite_manifest".to_string();
    }
    if manifest.id.is_empty() {
        manifest.id = new_id("eval_suite");
    }
    Ok(manifest)
}

fn load_replay_fixture(path: &Path) -> Result<ReplayFixture, VmError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| VmError::Runtime(format!("failed to read replay fixture: {e}")))?;
    serde_json::from_str(&content)
        .map_err(|e| VmError::Runtime(format!("failed to parse replay fixture: {e}")))
}

fn resolve_manifest_path(base_dir: Option<&Path>, path: &str) -> PathBuf {
    let path_buf = PathBuf::from(path);
    if path_buf.is_absolute() {
        path_buf
    } else if let Some(base_dir) = base_dir {
        base_dir.join(path_buf)
    } else {
        path_buf
    }
}

pub fn evaluate_run_suite_manifest(
    manifest: &EvalSuiteManifest,
) -> Result<ReplayEvalSuiteReport, VmError> {
    let base_dir = manifest.base_dir.as_deref().map(Path::new);
    let mut reports = Vec::new();
    for case in &manifest.cases {
        let run_path = resolve_manifest_path(base_dir, &case.run_path);
        let run = load_run_record(&run_path)?;
        let fixture = match &case.fixture_path {
            Some(path) => load_replay_fixture(&resolve_manifest_path(base_dir, path))?,
            None => run
                .replay_fixture
                .clone()
                .unwrap_or_else(|| replay_fixture_from_run(&run)),
        };
        let eval = evaluate_run_against_fixture(&run, &fixture);
        let mut pass = eval.pass;
        let mut failures = eval.failures;
        let comparison = match &case.compare_to {
            Some(path) => {
                let baseline_path = resolve_manifest_path(base_dir, path);
                let baseline = load_run_record(&baseline_path)?;
                let diff = diff_run_records(&baseline, &run);
                if !diff.identical {
                    pass = false;
                    failures.push(format!(
                        "run differs from baseline {} with {} stage changes",
                        baseline_path.display(),
                        diff.stage_diffs.len()
                    ));
                }
                Some(diff)
            }
            None => None,
        };
        reports.push(ReplayEvalCaseReport {
            run_id: run.id.clone(),
            workflow_id: run.workflow_id.clone(),
            label: case.label.clone(),
            pass,
            failures,
            stage_count: eval.stage_count,
            source_path: Some(run_path.display().to_string()),
            comparison,
        });
    }
    let total = reports.len();
    let passed = reports.iter().filter(|report| report.pass).count();
    let failed = total.saturating_sub(passed);
    Ok(ReplayEvalSuiteReport {
        pass: failed == 0,
        total,
        passed,
        failed,
        cases: reports,
    })
}

pub fn render_unified_diff(path: Option<&str>, before: &str, after: &str) -> String {
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    let mut table = vec![vec![0usize; after_lines.len() + 1]; before_lines.len() + 1];
    for i in (0..before_lines.len()).rev() {
        for j in (0..after_lines.len()).rev() {
            table[i][j] = if before_lines[i] == after_lines[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }

    let mut diff = String::new();
    let file = path.unwrap_or("artifact");
    diff.push_str(&format!("--- a/{file}\n+++ b/{file}\n"));
    let mut i = 0;
    let mut j = 0;
    while i < before_lines.len() && j < after_lines.len() {
        if before_lines[i] == after_lines[j] {
            diff.push_str(&format!(" {}\n", before_lines[i]));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            diff.push_str(&format!("-{}\n", before_lines[i]));
            i += 1;
        } else {
            diff.push_str(&format!("+{}\n", after_lines[j]));
            j += 1;
        }
    }
    while i < before_lines.len() {
        diff.push_str(&format!("-{}\n", before_lines[i]));
        i += 1;
    }
    while j < after_lines.len() {
        diff.push_str(&format!("+{}\n", after_lines[j]));
        j += 1;
    }
    diff
}

pub fn save_run_record(run: &RunRecord, path: Option<&str>) -> Result<String, VmError> {
    let path = path
        .map(PathBuf::from)
        .unwrap_or_else(|| default_run_dir().join(format!("{}.json", run.id)));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| VmError::Runtime(format!("failed to create run directory: {e}")))?;
    }
    let json = serde_json::to_string_pretty(run)
        .map_err(|e| VmError::Runtime(format!("failed to encode run record: {e}")))?;
    // Atomic write: write to .tmp then rename to prevent corruption on kill.
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &json)
        .map_err(|e| VmError::Runtime(format!("failed to persist run record: {e}")))?;
    std::fs::rename(&tmp_path, &path).map_err(|e| {
        // Fallback: if rename fails (cross-device), write directly.
        let _ = std::fs::write(&path, &json);
        VmError::Runtime(format!("failed to finalize run record: {e}"))
    })?;
    Ok(path.to_string_lossy().to_string())
}

pub fn load_run_record(path: &Path) -> Result<RunRecord, VmError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| VmError::Runtime(format!("failed to read run record: {e}")))?;
    serde_json::from_str(&content)
        .map_err(|e| VmError::Runtime(format!("failed to parse run record: {e}")))
}

pub fn replay_fixture_from_run(run: &RunRecord) -> ReplayFixture {
    ReplayFixture {
        type_name: "replay_fixture".to_string(),
        id: new_id("fixture"),
        source_run_id: run.id.clone(),
        workflow_id: run.workflow_id.clone(),
        workflow_name: run.workflow_name.clone(),
        created_at: now_rfc3339(),
        expected_status: run.status.clone(),
        stage_assertions: run
            .stages
            .iter()
            .map(|stage| ReplayStageAssertion {
                node_id: stage.node_id.clone(),
                expected_status: stage.status.clone(),
                expected_outcome: stage.outcome.clone(),
                expected_branch: stage.branch.clone(),
                required_artifact_kinds: stage
                    .artifacts
                    .iter()
                    .map(|artifact| artifact.kind.clone())
                    .collect(),
                visible_text_contains: stage
                    .visible_text
                    .as_ref()
                    .filter(|text| !text.is_empty())
                    .map(|text| text.chars().take(80).collect()),
            })
            .collect(),
    }
}

pub fn evaluate_run_against_fixture(run: &RunRecord, fixture: &ReplayFixture) -> ReplayEvalReport {
    let mut failures = Vec::new();
    if run.status != fixture.expected_status {
        failures.push(format!(
            "run status mismatch: expected {}, got {}",
            fixture.expected_status, run.status
        ));
    }
    for assertion in &fixture.stage_assertions {
        let Some(stage) = run
            .stages
            .iter()
            .find(|stage| stage.node_id == assertion.node_id)
        else {
            failures.push(format!("missing stage {}", assertion.node_id));
            continue;
        };
        if stage.status != assertion.expected_status {
            failures.push(format!(
                "stage {} status mismatch: expected {}, got {}",
                assertion.node_id, assertion.expected_status, stage.status
            ));
        }
        if stage.outcome != assertion.expected_outcome {
            failures.push(format!(
                "stage {} outcome mismatch: expected {}, got {}",
                assertion.node_id, assertion.expected_outcome, stage.outcome
            ));
        }
        if stage.branch != assertion.expected_branch {
            failures.push(format!(
                "stage {} branch mismatch: expected {:?}, got {:?}",
                assertion.node_id, assertion.expected_branch, stage.branch
            ));
        }
        for required_kind in &assertion.required_artifact_kinds {
            if !stage
                .artifacts
                .iter()
                .any(|artifact| &artifact.kind == required_kind)
            {
                failures.push(format!(
                    "stage {} missing artifact kind {}",
                    assertion.node_id, required_kind
                ));
            }
        }
        if let Some(snippet) = &assertion.visible_text_contains {
            let actual = stage.visible_text.clone().unwrap_or_default();
            if !actual.contains(snippet) {
                failures.push(format!(
                    "stage {} visible text does not contain expected snippet {:?}",
                    assertion.node_id, snippet
                ));
            }
        }
    }

    ReplayEvalReport {
        pass: failures.is_empty(),
        failures,
        stage_count: run.stages.len(),
    }
}

pub fn evaluate_run_suite(
    cases: Vec<(RunRecord, ReplayFixture, Option<String>)>,
) -> ReplayEvalSuiteReport {
    let mut reports = Vec::new();
    for (run, fixture, source_path) in cases {
        let report = evaluate_run_against_fixture(&run, &fixture);
        reports.push(ReplayEvalCaseReport {
            run_id: run.id.clone(),
            workflow_id: run.workflow_id.clone(),
            label: None,
            pass: report.pass,
            failures: report.failures,
            stage_count: report.stage_count,
            source_path,
            comparison: None,
        });
    }
    let total = reports.len();
    let passed = reports.iter().filter(|report| report.pass).count();
    let failed = total.saturating_sub(passed);
    ReplayEvalSuiteReport {
        pass: failed == 0,
        total,
        passed,
        failed,
        cases: reports,
    }
}

pub fn diff_run_records(left: &RunRecord, right: &RunRecord) -> RunDiffReport {
    let mut stage_diffs = Vec::new();
    let mut all_node_ids = BTreeSet::new();
    all_node_ids.extend(left.stages.iter().map(|stage| stage.node_id.clone()));
    all_node_ids.extend(right.stages.iter().map(|stage| stage.node_id.clone()));

    for node_id in all_node_ids {
        let left_stage = left.stages.iter().find(|stage| stage.node_id == node_id);
        let right_stage = right.stages.iter().find(|stage| stage.node_id == node_id);
        match (left_stage, right_stage) {
            (Some(_), None) => stage_diffs.push(RunStageDiffRecord {
                node_id,
                change: "removed".to_string(),
                details: vec!["stage missing from right run".to_string()],
            }),
            (None, Some(_)) => stage_diffs.push(RunStageDiffRecord {
                node_id,
                change: "added".to_string(),
                details: vec!["stage missing from left run".to_string()],
            }),
            (Some(left_stage), Some(right_stage)) => {
                let mut details = Vec::new();
                if left_stage.status != right_stage.status {
                    details.push(format!(
                        "status: {} -> {}",
                        left_stage.status, right_stage.status
                    ));
                }
                if left_stage.outcome != right_stage.outcome {
                    details.push(format!(
                        "outcome: {} -> {}",
                        left_stage.outcome, right_stage.outcome
                    ));
                }
                if left_stage.branch != right_stage.branch {
                    details.push(format!(
                        "branch: {:?} -> {:?}",
                        left_stage.branch, right_stage.branch
                    ));
                }
                if left_stage.produced_artifact_ids.len() != right_stage.produced_artifact_ids.len()
                {
                    details.push(format!(
                        "produced_artifacts: {} -> {}",
                        left_stage.produced_artifact_ids.len(),
                        right_stage.produced_artifact_ids.len()
                    ));
                }
                if left_stage.artifacts.len() != right_stage.artifacts.len() {
                    details.push(format!(
                        "artifact_records: {} -> {}",
                        left_stage.artifacts.len(),
                        right_stage.artifacts.len()
                    ));
                }
                if !details.is_empty() {
                    stage_diffs.push(RunStageDiffRecord {
                        node_id,
                        change: "changed".to_string(),
                        details,
                    });
                }
            }
            (None, None) => {}
        }
    }

    let status_changed = left.status != right.status;
    let identical = !status_changed
        && stage_diffs.is_empty()
        && left.transitions.len() == right.transitions.len()
        && left.artifacts.len() == right.artifacts.len()
        && left.checkpoints.len() == right.checkpoints.len();

    RunDiffReport {
        left_run_id: left.id.clone(),
        right_run_id: right.id.clone(),
        identical,
        status_changed,
        left_status: left.status.clone(),
        right_status: right.status.clone(),
        stage_diffs,
        transition_count_delta: right.transitions.len() as isize - left.transitions.len() as isize,
        artifact_count_delta: right.artifacts.len() as isize - left.artifacts.len() as isize,
        checkpoint_count_delta: right.checkpoints.len() as isize - left.checkpoints.len() as isize,
    }
}

pub fn push_execution_policy(policy: CapabilityPolicy) {
    EXECUTION_POLICY_STACK.with(|stack| stack.borrow_mut().push(policy));
}

pub fn pop_execution_policy() {
    EXECUTION_POLICY_STACK.with(|stack| {
        stack.borrow_mut().pop();
    });
}

pub fn current_execution_policy() -> Option<CapabilityPolicy> {
    EXECUTION_POLICY_STACK.with(|stack| stack.borrow().last().cloned())
}

pub fn current_tool_metadata(tool: &str) -> Option<ToolRuntimePolicyMetadata> {
    current_execution_policy().and_then(|policy| policy.tool_metadata.get(tool).cloned())
}

fn policy_allows_tool(policy: &CapabilityPolicy, tool: &str) -> bool {
    policy.tools.is_empty() || policy.tools.iter().any(|allowed| allowed == tool)
}

fn policy_allows_capability(policy: &CapabilityPolicy, capability: &str, op: &str) -> bool {
    policy.capabilities.is_empty()
        || policy
            .capabilities
            .get(capability)
            .is_some_and(|ops| ops.is_empty() || ops.iter().any(|allowed| allowed == op))
}

fn policy_allows_side_effect(policy: &CapabilityPolicy, requested: &str) -> bool {
    fn rank(v: &str) -> usize {
        match v {
            "none" => 0,
            "read_only" => 1,
            "workspace_write" => 2,
            "process_exec" => 3,
            "network" => 4,
            _ => 5,
        }
    }
    policy
        .side_effect_level
        .as_ref()
        .map(|allowed| rank(allowed) >= rank(requested))
        .unwrap_or(true)
}

fn reject_policy(reason: String) -> Result<(), VmError> {
    Err(VmError::CategorizedError {
        message: reason,
        category: crate::value::ErrorCategory::ToolRejected,
    })
}

fn fallback_mutation_classification(tool_name: &str) -> String {
    let lower = tool_name.to_ascii_lowercase();
    if lower.starts_with("mcp_") {
        return "host_defined".to_string();
    }
    if lower == "exec"
        || lower == "shell"
        || lower == "exec_at"
        || lower == "shell_at"
        || lower == "run"
        || lower.starts_with("run_")
    {
        return "ambient_side_effect".to_string();
    }
    if lower.starts_with("delete")
        || lower.starts_with("remove")
        || lower.starts_with("move")
        || lower.starts_with("rename")
    {
        return "destructive".to_string();
    }
    if lower.contains("write")
        || lower.contains("edit")
        || lower.contains("patch")
        || lower.contains("create")
        || lower.contains("scaffold")
        || lower.starts_with("insert")
        || lower.starts_with("replace")
        || lower == "add_import"
    {
        return "apply_workspace".to_string();
    }
    "read_only".to_string()
}

pub fn current_tool_mutation_classification(tool_name: &str) -> String {
    current_tool_metadata(tool_name)
        .and_then(|metadata| metadata.mutation_classification)
        .unwrap_or_else(|| fallback_mutation_classification(tool_name))
}

pub fn current_tool_declared_paths(tool_name: &str, args: &serde_json::Value) -> Vec<String> {
    let Some(map) = args.as_object() else {
        return Vec::new();
    };
    let path_keys = current_tool_metadata(tool_name)
        .map(|metadata| metadata.path_params)
        .filter(|keys| !keys.is_empty())
        .unwrap_or_else(|| {
            vec![
                "path".to_string(),
                "file".to_string(),
                "cwd".to_string(),
                "repo".to_string(),
                "target".to_string(),
                "destination".to_string(),
            ]
        });
    let mut paths = Vec::new();
    for key in path_keys {
        if let Some(value) = map.get(&key).and_then(|value| value.as_str()) {
            if !value.is_empty() {
                paths.push(value.to_string());
            }
        }
    }
    if let Some(items) = map.get("paths").and_then(|value| value.as_array()) {
        for item in items {
            if let Some(value) = item.as_str() {
                if !value.is_empty() {
                    paths.push(value.to_string());
                }
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

pub fn enforce_current_policy_for_builtin(name: &str, args: &[VmValue]) -> Result<(), VmError> {
    let Some(policy) = current_execution_policy() else {
        return Ok(());
    };
    match name {
        "read" | "read_file" => {
            if !policy_allows_tool(&policy, name)
                || !policy_allows_capability(&policy, "workspace", "read_text")
            {
                return reject_policy(format!(
                    "builtin '{name}' exceeds workspace.read_text ceiling"
                ));
            }
        }
        "search" | "list_dir" => {
            if !policy_allows_tool(&policy, name)
                || !policy_allows_capability(&policy, "workspace", "list")
            {
                return reject_policy(format!("builtin '{name}' exceeds workspace.list ceiling"));
            }
        }
        "file_exists" | "stat" => {
            if !policy_allows_capability(&policy, "workspace", "exists") {
                return reject_policy(format!("builtin '{name}' exceeds workspace.exists ceiling"));
            }
        }
        "edit" | "write_file" | "append_file" | "mkdir" | "copy_file" => {
            if !policy_allows_tool(&policy, "edit")
                || !policy_allows_capability(&policy, "workspace", "write_text")
                || !policy_allows_side_effect(&policy, "workspace_write")
            {
                return reject_policy(format!("builtin '{name}' exceeds workspace write ceiling"));
            }
        }
        "delete_file" => {
            if !policy_allows_capability(&policy, "workspace", "delete")
                || !policy_allows_side_effect(&policy, "workspace_write")
            {
                return reject_policy(
                    "builtin 'delete_file' exceeds workspace.delete ceiling".to_string(),
                );
            }
        }
        "apply_edit" => {
            if !policy_allows_capability(&policy, "workspace", "apply_edit")
                || !policy_allows_side_effect(&policy, "workspace_write")
            {
                return reject_policy(
                    "builtin 'apply_edit' exceeds workspace.apply_edit ceiling".to_string(),
                );
            }
        }
        "exec" | "exec_at" | "shell" | "shell_at" | "run_command" => {
            if !policy_allows_tool(&policy, "run")
                || !policy_allows_capability(&policy, "process", "exec")
                || !policy_allows_side_effect(&policy, "process_exec")
            {
                return reject_policy(format!("builtin '{name}' exceeds process.exec ceiling"));
            }
        }
        "http_get" | "http_post" | "http_put" | "http_patch" | "http_delete" | "http_request" => {
            if !policy_allows_side_effect(&policy, "network") {
                return reject_policy(format!("builtin '{name}' exceeds network ceiling"));
            }
        }
        "mcp_connect"
        | "mcp_call"
        | "mcp_list_tools"
        | "mcp_list_resources"
        | "mcp_list_resource_templates"
        | "mcp_read_resource"
        | "mcp_list_prompts"
        | "mcp_get_prompt"
        | "mcp_server_info"
        | "mcp_disconnect" => {
            if !policy_allows_tool(&policy, "run")
                || !policy_allows_capability(&policy, "process", "exec")
                || !policy_allows_side_effect(&policy, "process_exec")
            {
                return reject_policy(format!("builtin '{name}' exceeds process.exec ceiling"));
            }
        }
        "host_call" => {
            let name = args.first().map(|v| v.display()).unwrap_or_default();
            let Some((capability, op)) = name.split_once('.') else {
                return reject_policy(format!(
                    "host_call '{name}' must use capability.operation naming"
                ));
            };
            if !policy_allows_capability(&policy, capability, op) {
                return reject_policy(format!(
                    "host_call {capability}.{op} exceeds capability ceiling"
                ));
            }
            let requested_side_effect = match (capability, op) {
                ("workspace", "write_text" | "apply_edit" | "delete") => "workspace_write",
                ("process", "exec") => "process_exec",
                _ => "read_only",
            };
            if !policy_allows_side_effect(&policy, requested_side_effect) {
                return reject_policy(format!(
                    "host_call {capability}.{op} exceeds side-effect ceiling"
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

pub fn enforce_current_policy_for_bridge_builtin(name: &str) -> Result<(), VmError> {
    if current_execution_policy().is_some() {
        return reject_policy(format!(
            "bridged builtin '{name}' exceeds execution policy; declare an explicit capability/tool surface instead"
        ));
    }
    Ok(())
}

pub fn enforce_current_policy_for_tool(tool_name: &str) -> Result<(), VmError> {
    let Some(policy) = current_execution_policy() else {
        return Ok(());
    };
    if !policy_allows_tool(&policy, tool_name) {
        return reject_policy(format!("tool '{tool_name}' exceeds tool ceiling"));
    }
    if let Some(metadata) = policy.tool_metadata.get(tool_name) {
        for (capability, ops) in &metadata.capabilities {
            for op in ops {
                if !policy_allows_capability(&policy, capability, op) {
                    return reject_policy(format!(
                        "tool '{tool_name}' exceeds capability ceiling: {capability}.{op}"
                    ));
                }
            }
        }
        if let Some(side_effect_level) = metadata.side_effect_level.as_deref() {
            if !policy_allows_side_effect(&policy, side_effect_level) {
                return reject_policy(format!(
                    "tool '{tool_name}' exceeds side-effect ceiling: {side_effect_level}"
                ));
            }
        }
    }
    Ok(())
}

fn compact_transcript(transcript: &VmValue, keep_last: usize) -> Option<VmValue> {
    let dict = transcript.as_dict()?;
    let messages = match dict.get("messages") {
        Some(VmValue::List(list)) => list.iter().cloned().collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    let retained = messages
        .into_iter()
        .rev()
        .take(keep_last)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    let mut compacted = dict.clone();
    compacted.insert(
        "messages".to_string(),
        VmValue::List(Rc::new(retained.clone())),
    );
    compacted.insert(
        "events".to_string(),
        VmValue::List(Rc::new(
            crate::llm::helpers::transcript_events_from_messages(&retained),
        )),
    );
    Some(VmValue::Dict(Rc::new(compacted)))
}

fn redact_transcript_visibility(transcript: &VmValue, visibility: Option<&str>) -> Option<VmValue> {
    let Some(visibility) = visibility else {
        return Some(transcript.clone());
    };
    if visibility != "public" && visibility != "public_only" {
        return Some(transcript.clone());
    }
    let dict = transcript.as_dict()?;
    let public_messages = match dict.get("messages") {
        Some(VmValue::List(list)) => list
            .iter()
            .filter(|message| {
                message
                    .as_dict()
                    .and_then(|d| d.get("role"))
                    .map(|v| v.display())
                    .map(|role| role != "tool_result")
                    .unwrap_or(true)
            })
            .cloned()
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    let public_events = match dict.get("events") {
        Some(VmValue::List(list)) => list
            .iter()
            .filter(|event| {
                event
                    .as_dict()
                    .and_then(|d| d.get("visibility"))
                    .map(|v| v.display())
                    .map(|value| value == "public")
                    .unwrap_or(true)
            })
            .cloned()
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    let mut redacted = dict.clone();
    redacted.insert(
        "messages".to_string(),
        VmValue::List(Rc::new(public_messages)),
    );
    redacted.insert("events".to_string(), VmValue::List(Rc::new(public_events)));
    Some(VmValue::Dict(Rc::new(redacted)))
}

pub(crate) fn apply_input_transcript_policy(
    transcript: Option<VmValue>,
    policy: &TranscriptPolicy,
) -> Option<VmValue> {
    let mut transcript = transcript;
    match policy.mode.as_deref() {
        Some("reset") => return None,
        Some("fork") => {
            if let Some(VmValue::Dict(dict)) = transcript.as_ref() {
                let mut forked = dict.as_ref().clone();
                forked.insert(
                    "id".to_string(),
                    VmValue::String(Rc::from(new_id("transcript"))),
                );
                transcript = Some(VmValue::Dict(Rc::new(forked)));
            }
        }
        _ => {}
    }
    if policy.compact {
        let keep_last = policy.keep_last.unwrap_or(6);
        transcript = transcript.and_then(|value| compact_transcript(&value, keep_last));
    }
    transcript
}

fn apply_output_transcript_policy(
    transcript: Option<VmValue>,
    policy: &TranscriptPolicy,
) -> Option<VmValue> {
    let mut transcript = transcript;
    if policy.compact {
        let keep_last = policy.keep_last.unwrap_or(6);
        transcript = transcript.and_then(|value| compact_transcript(&value, keep_last));
    }
    transcript.and_then(|value| redact_transcript_visibility(&value, policy.visibility.as_deref()))
}

pub async fn execute_stage_node(
    node_id: &str,
    node: &WorkflowNode,
    task: &str,
    artifacts: &[ArtifactRecord],
    transcript: Option<VmValue>,
) -> Result<(serde_json::Value, Vec<ArtifactRecord>, Option<VmValue>), VmError> {
    let mut selection_policy = node.context_policy.clone();
    if selection_policy.include_kinds.is_empty() && !node.input_contract.input_kinds.is_empty() {
        selection_policy.include_kinds = node.input_contract.input_kinds.clone();
    }
    let selected = select_artifacts_adaptive(artifacts.to_vec(), &selection_policy);
    let rendered_context = render_artifacts_context(&selected, &node.context_policy);
    let transcript = apply_input_transcript_policy(transcript, &node.transcript_policy);
    if node.input_contract.require_transcript && transcript.is_none() {
        return Err(VmError::Runtime(format!(
            "workflow stage {node_id} requires transcript input"
        )));
    }
    if let Some(min_inputs) = node.input_contract.min_inputs {
        if selected.len() < min_inputs {
            return Err(VmError::Runtime(format!(
                "workflow stage {node_id} requires at least {min_inputs} input artifacts"
            )));
        }
    }
    if let Some(max_inputs) = node.input_contract.max_inputs {
        if selected.len() > max_inputs {
            return Err(VmError::Runtime(format!(
                "workflow stage {node_id} accepts at most {max_inputs} input artifacts"
            )));
        }
    }
    let prompt = if rendered_context.is_empty() {
        task.to_string()
    } else {
        format!(
            "{rendered_context}\n\n{}:\n{task}",
            node.task_label
                .clone()
                .unwrap_or_else(|| "Task".to_string())
        )
    };

    let tool_format = std::env::var("HARN_AGENT_TOOL_FORMAT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "text".to_string());
    let mut llm_result = if node.kind == "verify" {
        if let Some(command) = node
            .verify
            .as_ref()
            .and_then(|verify| verify.as_object())
            .and_then(|verify| verify.get("command"))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let mut process = if cfg!(target_os = "windows") {
                let mut cmd = tokio::process::Command::new("cmd");
                cmd.arg("/C").arg(command);
                cmd
            } else {
                let mut cmd = tokio::process::Command::new("/bin/sh");
                cmd.arg("-lc").arg(command);
                cmd
            };
            process.stdin(std::process::Stdio::null());
            if let Some(context) = crate::stdlib::process::current_execution_context() {
                if let Some(cwd) = context.cwd.filter(|cwd| !cwd.is_empty()) {
                    process.current_dir(cwd);
                }
                if !context.env.is_empty() {
                    process.envs(context.env);
                }
            }
            let output = process
                .output()
                .await
                .map_err(|e| VmError::Runtime(format!("workflow verify exec failed: {e}")))?;
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let combined = if stderr.is_empty() {
                stdout.clone()
            } else if stdout.is_empty() {
                stderr.clone()
            } else {
                format!("{stdout}\n{stderr}")
            };
            serde_json::json!({
                "status": "completed",
                "text": combined,
                "visible_text": combined,
                "command": command,
                "stdout": stdout,
                "stderr": stderr,
                "exit_status": output.status.code().unwrap_or(-1),
                "success": output.status.success(),
            })
        } else {
            serde_json::json!({
                "status": "completed",
                "text": "",
                "visible_text": "",
            })
        }
    } else {
        let mut options = BTreeMap::new();
        if let Some(provider) = &node.model_policy.provider {
            options.insert(
                "provider".to_string(),
                VmValue::String(Rc::from(provider.clone())),
            );
        }
        if let Some(model) = &node.model_policy.model {
            options.insert(
                "model".to_string(),
                VmValue::String(Rc::from(model.clone())),
            );
        }
        if let Some(model_tier) = &node.model_policy.model_tier {
            options.insert(
                "model_tier".to_string(),
                VmValue::String(Rc::from(model_tier.clone())),
            );
        }
        if let Some(temperature) = node.model_policy.temperature {
            options.insert("temperature".to_string(), VmValue::Float(temperature));
        }
        if let Some(max_tokens) = node.model_policy.max_tokens {
            options.insert("max_tokens".to_string(), VmValue::Int(max_tokens));
        }
        let tool_names = workflow_tool_names(&node.tools);
        let tools_value = node.raw_tools.clone().or_else(|| {
            if matches!(node.tools, serde_json::Value::Null) {
                None
            } else {
                Some(crate::stdlib::json_to_vm_value(&node.tools))
            }
        });
        if tools_value.is_some() && !tool_names.is_empty() {
            options.insert("tools".to_string(), tools_value.unwrap_or(VmValue::Nil));
        }
        if let Some(transcript) = transcript.clone() {
            options.insert("transcript".to_string(), transcript);
        }

        let args = vec![
            VmValue::String(Rc::from(prompt.clone())),
            node.system
                .clone()
                .map(|s| VmValue::String(Rc::from(s)))
                .unwrap_or(VmValue::Nil),
            VmValue::Dict(Rc::new(options)),
        ];
        let mut opts = extract_llm_options(&args)?;

        if node.mode.as_deref() == Some("agent") || !tool_names.is_empty() {
            crate::llm::run_agent_loop_internal(
                &mut opts,
                crate::llm::AgentLoopConfig {
                    persistent: true,
                    max_iterations: 12,
                    max_nudges: 3,
                    nudge: None,
                    done_sentinel: node.done_sentinel.clone(),
                    break_unless_phase: None,
                    tool_retries: 0,
                    tool_backoff_ms: 1000,
                    tool_format: tool_format.clone(),
                    auto_compact: None,
                    context_callback: None,
                    policy: None,
                    daemon: false,
                    llm_retries: 2,
                    llm_backoff_ms: 2000,
                },
            )
            .await?
        } else {
            let result = vm_call_llm_full(&opts).await?;
            crate::llm::agent_loop_result_from_llm(&result, opts)
        }
    };
    if let Some(payload) = llm_result.as_object_mut() {
        payload.insert("prompt".to_string(), serde_json::json!(prompt));
        payload.insert(
            "system_prompt".to_string(),
            serde_json::json!(node.system.clone().unwrap_or_default()),
        );
        payload.insert(
            "rendered_context".to_string(),
            serde_json::json!(rendered_context),
        );
        payload.insert(
            "selected_artifact_ids".to_string(),
            serde_json::json!(selected
                .iter()
                .map(|artifact| artifact.id.clone())
                .collect::<Vec<_>>()),
        );
        payload.insert(
            "selected_artifact_titles".to_string(),
            serde_json::json!(selected
                .iter()
                .map(|artifact| artifact.title.clone())
                .collect::<Vec<_>>()),
        );
        payload.insert(
            "tool_calling_mode".to_string(),
            serde_json::json!(tool_format.clone()),
        );
    }

    let visible_text = llm_result["text"].as_str().unwrap_or_default().to_string();
    let transcript = llm_result
        .get("transcript")
        .cloned()
        .map(|value| crate::stdlib::json_to_vm_value(&value));
    let transcript = apply_output_transcript_policy(transcript, &node.transcript_policy);
    let output_kind = node
        .output_contract
        .output_kinds
        .first()
        .cloned()
        .unwrap_or_else(|| {
            if node.kind == "verify" {
                "verification_result".to_string()
            } else {
                "artifact".to_string()
            }
        });
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "input_artifact_ids".to_string(),
        serde_json::json!(selected
            .iter()
            .map(|artifact| artifact.id.clone())
            .collect::<Vec<_>>()),
    );
    metadata.insert("node_kind".to_string(), serde_json::json!(node.kind));
    let artifact = ArtifactRecord {
        type_name: "artifact".to_string(),
        id: new_id("artifact"),
        kind: output_kind,
        title: Some(format!("stage {node_id} output")),
        text: Some(visible_text),
        data: Some(llm_result.clone()),
        source: Some(node_id.to_string()),
        created_at: now_rfc3339(),
        freshness: Some("fresh".to_string()),
        priority: None,
        lineage: selected
            .iter()
            .map(|artifact| artifact.id.clone())
            .collect(),
        relevance: Some(1.0),
        estimated_tokens: None,
        stage: Some(node_id.to_string()),
        metadata,
    }
    .normalize();

    Ok((llm_result, vec![artifact], transcript))
}

pub fn next_nodes_for(
    graph: &WorkflowGraph,
    current: &str,
    branch: Option<&str>,
) -> Vec<WorkflowEdge> {
    let mut matching: Vec<WorkflowEdge> = graph
        .edges
        .iter()
        .filter(|edge| edge.from == current && edge.branch.as_deref() == branch)
        .cloned()
        .collect();
    if matching.is_empty() {
        matching = graph
            .edges
            .iter()
            .filter(|edge| edge.from == current && edge.branch.is_none())
            .cloned()
            .collect();
    }
    matching
}

pub fn next_node_for(graph: &WorkflowGraph, current: &str, branch: &str) -> Option<String> {
    next_nodes_for(graph, current, Some(branch))
        .into_iter()
        .next()
        .map(|edge| edge.to)
}

pub fn append_audit_entry(
    graph: &mut WorkflowGraph,
    op: &str,
    node_id: Option<String>,
    reason: Option<String>,
    metadata: BTreeMap<String, serde_json::Value>,
) {
    graph.audit_log.push(WorkflowAuditEntry {
        id: new_id("audit"),
        op: op.to_string(),
        node_id,
        timestamp: now_rfc3339(),
        reason,
        metadata,
    });
}

pub fn builtin_ceiling() -> CapabilityPolicy {
    CapabilityPolicy {
        // Runtime-owned ceiling is capability-based, not product-tool-based.
        // Integrators define concrete tool surfaces in workflow graphs / registries.
        tools: Vec::new(),
        capabilities: BTreeMap::from([
            (
                "workspace".to_string(),
                vec![
                    "read_text".to_string(),
                    "write_text".to_string(),
                    "apply_edit".to_string(),
                    "delete".to_string(),
                    "exists".to_string(),
                    "list".to_string(),
                ],
            ),
            ("process".to_string(), vec!["exec".to_string()]),
        ]),
        workspace_roots: Vec::new(),
        side_effect_level: Some("network".to_string()),
        recursion_limit: Some(8),
        tool_arg_constraints: Vec::new(),
        tool_metadata: BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_intersection_rejects_privilege_expansion() {
        let ceiling = CapabilityPolicy {
            tools: vec!["read".to_string()],
            side_effect_level: Some("read_only".to_string()),
            recursion_limit: Some(2),
            ..Default::default()
        };
        let requested = CapabilityPolicy {
            tools: vec!["read".to_string(), "edit".to_string()],
            ..Default::default()
        };
        let error = ceiling.intersect(&requested).unwrap_err();
        assert!(error.contains("host ceiling"));
    }

    #[test]
    fn mutation_session_normalize_fills_defaults() {
        let normalized = MutationSessionRecord::default().normalize();
        assert!(normalized.session_id.starts_with("session_"));
        assert_eq!(normalized.mutation_scope, "read_only");
        assert_eq!(normalized.approval_mode, "host_enforced");
    }

    #[test]
    fn install_current_mutation_session_round_trips() {
        install_current_mutation_session(Some(MutationSessionRecord {
            session_id: "session_test".to_string(),
            mutation_scope: "apply_workspace".to_string(),
            approval_mode: "explicit".to_string(),
            ..Default::default()
        }));
        let current = current_mutation_session().expect("session installed");
        assert_eq!(current.session_id, "session_test");
        assert_eq!(current.mutation_scope, "apply_workspace");
        assert_eq!(current.approval_mode, "explicit");

        install_current_mutation_session(None);
        assert!(current_mutation_session().is_none());
    }

    #[test]
    fn active_execution_policy_rejects_unknown_bridge_builtin() {
        push_execution_policy(CapabilityPolicy {
            tools: vec!["read".to_string()],
            capabilities: BTreeMap::from([(
                "workspace".to_string(),
                vec!["read_text".to_string()],
            )]),
            side_effect_level: Some("read_only".to_string()),
            recursion_limit: Some(1),
            ..Default::default()
        });
        let error = enforce_current_policy_for_bridge_builtin("custom_host_builtin").unwrap_err();
        pop_execution_policy();
        assert!(matches!(
            error,
            VmError::CategorizedError {
                category: crate::value::ErrorCategory::ToolRejected,
                ..
            }
        ));
    }

    #[test]
    fn active_execution_policy_rejects_mcp_escape_hatch() {
        push_execution_policy(CapabilityPolicy {
            tools: vec!["read".to_string()],
            capabilities: BTreeMap::from([(
                "workspace".to_string(),
                vec!["read_text".to_string()],
            )]),
            side_effect_level: Some("read_only".to_string()),
            recursion_limit: Some(1),
            ..Default::default()
        });
        let error = enforce_current_policy_for_builtin("mcp_connect", &[]).unwrap_err();
        pop_execution_policy();
        assert!(matches!(
            error,
            VmError::CategorizedError {
                category: crate::value::ErrorCategory::ToolRejected,
                ..
            }
        ));
    }

    #[test]
    fn workflow_normalization_upgrades_legacy_act_verify_repair_shape() {
        let value = crate::stdlib::json_to_vm_value(&serde_json::json!({
            "name": "legacy",
            "act": {"mode": "llm"},
            "verify": {"kind": "verify"},
            "repair": {"mode": "agent"},
        }));
        let graph = normalize_workflow_value(&value).unwrap();
        assert_eq!(graph.type_name, "workflow_graph");
        assert!(graph.nodes.contains_key("act"));
        assert!(graph.nodes.contains_key("verify"));
        assert!(graph.nodes.contains_key("repair"));
        assert_eq!(graph.entry, "act");
    }

    #[test]
    fn workflow_normalization_accepts_tool_registry_nodes() {
        let value = crate::stdlib::json_to_vm_value(&serde_json::json!({
            "name": "registry_tools",
            "entry": "implement",
            "nodes": {
                "implement": {
                    "kind": "stage",
                    "mode": "agent",
                    "tools": {
                        "_type": "tool_registry",
                        "tools": [
                            {"name": "read", "description": "Read files"},
                            {"name": "run", "description": "Run commands"}
                        ]
                    }
                }
            },
            "edges": []
        }));
        let graph = normalize_workflow_value(&value).unwrap();
        let node = graph.nodes.get("implement").unwrap();
        assert_eq!(workflow_tool_names(&node.tools), vec!["read", "run"]);
    }

    #[test]
    fn artifact_selection_honors_budget_and_priority() {
        let policy = ContextPolicy {
            max_artifacts: Some(2),
            max_tokens: Some(30),
            prefer_recent: true,
            prefer_fresh: true,
            prioritize_kinds: vec!["verification_result".to_string()],
            ..Default::default()
        };
        let artifacts = vec![
            ArtifactRecord {
                type_name: "artifact".to_string(),
                id: "a".to_string(),
                kind: "summary".to_string(),
                text: Some("short".to_string()),
                relevance: Some(0.9),
                created_at: now_rfc3339(),
                ..Default::default()
            }
            .normalize(),
            ArtifactRecord {
                type_name: "artifact".to_string(),
                id: "b".to_string(),
                kind: "summary".to_string(),
                text: Some("this is a much larger artifact body".to_string()),
                relevance: Some(1.0),
                created_at: now_rfc3339(),
                ..Default::default()
            }
            .normalize(),
            ArtifactRecord {
                type_name: "artifact".to_string(),
                id: "c".to_string(),
                kind: "summary".to_string(),
                text: Some("tiny".to_string()),
                relevance: Some(0.5),
                created_at: now_rfc3339(),
                ..Default::default()
            }
            .normalize(),
        ];
        let selected = select_artifacts(artifacts, &policy);
        assert_eq!(selected.len(), 2);
        assert!(selected.iter().all(|artifact| artifact.kind == "summary"));
    }

    #[test]
    fn workflow_validation_rejects_condition_without_true_false_edges() {
        let graph = WorkflowGraph {
            entry: "gate".to_string(),
            nodes: BTreeMap::from([(
                "gate".to_string(),
                WorkflowNode {
                    id: Some("gate".to_string()),
                    kind: "condition".to_string(),
                    ..Default::default()
                },
            )]),
            edges: vec![WorkflowEdge {
                from: "gate".to_string(),
                to: "next".to_string(),
                branch: Some("true".to_string()),
                label: None,
            }],
            ..Default::default()
        };
        let report = validate_workflow(&graph, None);
        assert!(!report.valid);
        assert!(report
            .errors
            .iter()
            .any(|error| error.contains("true") && error.contains("false")));
    }

    #[test]
    fn replay_fixture_round_trip_passes() {
        let run = RunRecord {
            type_name: "run_record".to_string(),
            id: "run_1".to_string(),
            workflow_id: "wf".to_string(),
            workflow_name: Some("demo".to_string()),
            task: "demo".to_string(),
            status: "completed".to_string(),
            started_at: "1".to_string(),
            finished_at: Some("2".to_string()),
            parent_run_id: None,
            root_run_id: Some("run_1".to_string()),
            stages: vec![RunStageRecord {
                id: "stage_1".to_string(),
                node_id: "act".to_string(),
                kind: "stage".to_string(),
                status: "completed".to_string(),
                outcome: "success".to_string(),
                branch: Some("success".to_string()),
                started_at: "1".to_string(),
                finished_at: Some("2".to_string()),
                visible_text: Some("done".to_string()),
                private_reasoning: None,
                transcript: None,
                verification: None,
                usage: None,
                artifacts: vec![ArtifactRecord {
                    type_name: "artifact".to_string(),
                    id: "a1".to_string(),
                    kind: "summary".to_string(),
                    text: Some("done".to_string()),
                    created_at: "1".to_string(),
                    ..Default::default()
                }
                .normalize()],
                consumed_artifact_ids: vec![],
                produced_artifact_ids: vec!["a1".to_string()],
                attempts: vec![],
                metadata: BTreeMap::new(),
            }],
            transitions: vec![],
            checkpoints: vec![],
            pending_nodes: vec![],
            completed_nodes: vec!["act".to_string()],
            child_runs: vec![],
            artifacts: vec![],
            policy: CapabilityPolicy::default(),
            execution: None,
            transcript: None,
            usage: None,
            replay_fixture: None,
            trace_spans: vec![],
            metadata: BTreeMap::new(),
            persisted_path: None,
        };
        let fixture = replay_fixture_from_run(&run);
        let report = evaluate_run_against_fixture(&run, &fixture);
        assert!(report.pass);
        assert!(report.failures.is_empty());
    }

    #[test]
    fn replay_eval_suite_reports_failed_case() {
        let good = RunRecord {
            id: "run_good".to_string(),
            workflow_id: "wf".to_string(),
            status: "completed".to_string(),
            stages: vec![RunStageRecord {
                node_id: "act".to_string(),
                status: "completed".to_string(),
                outcome: "success".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let bad = RunRecord {
            id: "run_bad".to_string(),
            workflow_id: "wf".to_string(),
            status: "failed".to_string(),
            stages: vec![RunStageRecord {
                node_id: "act".to_string(),
                status: "failed".to_string(),
                outcome: "error".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let suite = evaluate_run_suite(vec![
            (
                good.clone(),
                replay_fixture_from_run(&good),
                Some("good.json".to_string()),
            ),
            (
                bad.clone(),
                replay_fixture_from_run(&good),
                Some("bad.json".to_string()),
            ),
        ]);
        assert!(!suite.pass);
        assert_eq!(suite.total, 2);
        assert_eq!(suite.failed, 1);
        assert!(suite.cases.iter().any(|case| !case.pass));
    }

    #[test]
    fn run_diff_reports_changed_stage() {
        let left = RunRecord {
            id: "left".to_string(),
            workflow_id: "wf".to_string(),
            status: "completed".to_string(),
            stages: vec![RunStageRecord {
                node_id: "act".to_string(),
                status: "completed".to_string(),
                outcome: "success".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let right = RunRecord {
            id: "right".to_string(),
            workflow_id: "wf".to_string(),
            status: "failed".to_string(),
            stages: vec![RunStageRecord {
                node_id: "act".to_string(),
                status: "failed".to_string(),
                outcome: "error".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let diff = diff_run_records(&left, &right);
        assert!(diff.status_changed);
        assert!(!diff.identical);
        assert_eq!(diff.stage_diffs.len(), 1);
    }

    #[test]
    fn eval_suite_manifest_can_fail_on_baseline_diff() {
        let temp_dir =
            std::env::temp_dir().join(format!("harn-eval-suite-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let baseline_path = temp_dir.join("baseline.json");
        let candidate_path = temp_dir.join("candidate.json");

        let baseline = RunRecord {
            id: "baseline".to_string(),
            workflow_id: "wf".to_string(),
            status: "completed".to_string(),
            stages: vec![RunStageRecord {
                node_id: "act".to_string(),
                status: "completed".to_string(),
                outcome: "success".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let candidate = RunRecord {
            id: "candidate".to_string(),
            workflow_id: "wf".to_string(),
            status: "failed".to_string(),
            stages: vec![RunStageRecord {
                node_id: "act".to_string(),
                status: "failed".to_string(),
                outcome: "error".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        save_run_record(&baseline, Some(baseline_path.to_str().unwrap())).unwrap();
        save_run_record(&candidate, Some(candidate_path.to_str().unwrap())).unwrap();

        let manifest = EvalSuiteManifest {
            base_dir: Some(temp_dir.display().to_string()),
            cases: vec![EvalSuiteCase {
                label: Some("candidate".to_string()),
                run_path: "candidate.json".to_string(),
                fixture_path: None,
                compare_to: Some("baseline.json".to_string()),
            }],
            ..Default::default()
        };
        let suite = evaluate_run_suite_manifest(&manifest).unwrap();
        assert!(!suite.pass);
        assert_eq!(suite.failed, 1);
        assert!(suite.cases[0].comparison.is_some());
        assert!(suite.cases[0]
            .failures
            .iter()
            .any(|failure| failure.contains("baseline")));
    }

    #[test]
    fn render_unified_diff_marks_removed_and_added_lines() {
        let diff = render_unified_diff(Some("src/main.rs"), "old\nsame", "new\nsame");
        assert!(diff.contains("--- a/src/main.rs"));
        assert!(diff.contains("+++ b/src/main.rs"));
        assert!(diff.contains("-old"));
        assert!(diff.contains("+new"));
        assert!(diff.contains(" same"));
    }

    #[test]
    fn execution_policy_rejects_process_exec_when_read_only() {
        push_execution_policy(CapabilityPolicy {
            side_effect_level: Some("read_only".to_string()),
            capabilities: BTreeMap::from([("process".to_string(), vec!["exec".to_string()])]),
            ..Default::default()
        });
        let result = enforce_current_policy_for_builtin("exec", &[]);
        pop_execution_policy();
        assert!(result.is_err());
    }

    #[test]
    fn execution_policy_rejects_unlisted_tool() {
        push_execution_policy(CapabilityPolicy {
            tools: vec!["read".to_string()],
            ..Default::default()
        });
        let result = enforce_current_policy_for_tool("edit");
        pop_execution_policy();
        assert!(result.is_err());
    }

    #[test]
    fn normalize_run_record_preserves_trace_spans() {
        let value = crate::stdlib::json_to_vm_value(&serde_json::json!({
            "_type": "run_record",
            "id": "run_trace",
            "workflow_id": "wf",
            "status": "completed",
            "started_at": "1",
            "trace_spans": [
                {
                    "span_id": 1,
                    "parent_id": null,
                    "kind": "pipeline",
                    "name": "workflow",
                    "start_ms": 0,
                    "duration_ms": 42,
                    "metadata": {"model": "demo"}
                }
            ]
        }));

        let run = normalize_run_record(&value).unwrap();
        assert_eq!(run.trace_spans.len(), 1);
        assert_eq!(run.trace_spans[0].kind, "pipeline");
        assert_eq!(
            run.trace_spans[0].metadata["model"],
            serde_json::json!("demo")
        );
    }

    // ── Tool hook tests ──────────────────────────────────────────────

    #[test]
    fn pre_tool_hook_deny_blocks_execution() {
        clear_tool_hooks();
        register_tool_hook(ToolHook {
            pattern: "dangerous_*".to_string(),
            pre: Some(Rc::new(|_name, _args| {
                PreToolAction::Deny("blocked by policy".to_string())
            })),
            post: None,
        });
        let result = run_pre_tool_hooks("dangerous_delete", &serde_json::json!({}));
        clear_tool_hooks();
        assert!(matches!(result, PreToolAction::Deny(_)));
    }

    #[test]
    fn pre_tool_hook_allow_passes_through() {
        clear_tool_hooks();
        register_tool_hook(ToolHook {
            pattern: "safe_*".to_string(),
            pre: Some(Rc::new(|_name, _args| PreToolAction::Allow)),
            post: None,
        });
        let result = run_pre_tool_hooks("safe_read", &serde_json::json!({}));
        clear_tool_hooks();
        assert!(matches!(result, PreToolAction::Allow));
    }

    #[test]
    fn pre_tool_hook_modify_rewrites_args() {
        clear_tool_hooks();
        register_tool_hook(ToolHook {
            pattern: "*".to_string(),
            pre: Some(Rc::new(|_name, _args| {
                PreToolAction::Modify(serde_json::json!({"path": "/sanitized"}))
            })),
            post: None,
        });
        let result = run_pre_tool_hooks("read_file", &serde_json::json!({"path": "/etc/passwd"}));
        clear_tool_hooks();
        match result {
            PreToolAction::Modify(args) => assert_eq!(args["path"], "/sanitized"),
            _ => panic!("expected Modify"),
        }
    }

    #[test]
    fn post_tool_hook_modifies_result() {
        clear_tool_hooks();
        register_tool_hook(ToolHook {
            pattern: "exec".to_string(),
            pre: None,
            post: Some(Rc::new(|_name, result| {
                if result.contains("SECRET") {
                    PostToolAction::Modify("[REDACTED]".to_string())
                } else {
                    PostToolAction::Pass
                }
            })),
        });
        let result = run_post_tool_hooks("exec", "output with SECRET data");
        let clean = run_post_tool_hooks("exec", "clean output");
        clear_tool_hooks();
        assert_eq!(result, "[REDACTED]");
        assert_eq!(clean, "clean output");
    }

    #[test]
    fn unmatched_hook_pattern_does_not_fire() {
        clear_tool_hooks();
        register_tool_hook(ToolHook {
            pattern: "exec".to_string(),
            pre: Some(Rc::new(|_name, _args| {
                PreToolAction::Deny("should not match".to_string())
            })),
            post: None,
        });
        let result = run_pre_tool_hooks("read_file", &serde_json::json!({}));
        clear_tool_hooks();
        assert!(matches!(result, PreToolAction::Allow));
    }

    #[test]
    fn glob_match_patterns() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("exec*", "exec_at"));
        assert!(glob_match("*_file", "read_file"));
        assert!(!glob_match("exec*", "read_file"));
        assert!(glob_match("read_file", "read_file"));
        assert!(!glob_match("read_file", "write_file"));
    }

    // ── Auto-compaction tests ────────────────────────────────────────

    #[test]
    fn microcompact_snips_large_output() {
        let large = "x".repeat(50_000);
        let result = microcompact_tool_output(&large, 10_000);
        assert!(result.len() < 15_000);
        assert!(result.contains("snipped"));
    }

    #[test]
    fn microcompact_preserves_small_output() {
        let small = "hello world";
        let result = microcompact_tool_output(small, 10_000);
        assert_eq!(result, small);
    }

    #[test]
    fn microcompact_preserves_strong_keyword_lines_without_file_line() {
        // Regression: diagnostic extraction used to require both a
        // file:line reference AND a keyword. Strong keywords like "FAIL"
        // and "panic" should preserve the line on their own, because they
        // carry signal even when they appear on narrative lines (Go's
        // "--- FAIL: TestName", Rust's "thread '...' panicked at ...",
        // pytest's "FAILED tests/..."). The exact patterns are language-
        // specific and don't belong in the VM — but the generic rule
        // "strong keywords count even without file:line" does.
        let mut output = String::new();
        for i in 0..100 {
            output.push_str(&format!("verbose progress line {i}\n"));
        }
        output.push_str("--- FAIL: TestEmpty (0.00s)\n");
        output.push_str("thread 'tests::test_foo' panicked at src/lib.rs:42:5\n");
        output.push_str("FAILED tests/test_parser.py::test_empty\n");
        for i in 0..100 {
            output.push_str(&format!("more output after failures {i}\n"));
        }
        let result = microcompact_tool_output(&output, 2_000);
        assert!(
            result.contains("--- FAIL: TestEmpty"),
            "strong 'FAIL' keyword should preserve the line:\n{result}"
        );
        assert!(
            result.contains("panicked at"),
            "strong 'panic' keyword should preserve the line:\n{result}"
        );
        assert!(
            result.contains("FAILED tests/test_parser.py"),
            "strong 'FAIL' keyword should preserve pytest-style lines too:\n{result}"
        );
    }

    #[test]
    fn auto_compact_messages_reduces_count() {
        let mut messages: Vec<serde_json::Value> = (0..20)
            .map(|i| serde_json::json!({"role": "user", "content": format!("message {i}")}))
            .collect();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let compacted = runtime.block_on(auto_compact_messages(
            &mut messages,
            &AutoCompactConfig {
                compact_strategy: CompactStrategy::Truncate,
                keep_last: 6,
                ..Default::default()
            },
            None,
        ));
        let summary = compacted.unwrap();
        assert!(summary.is_some());
        assert!(messages.len() <= 7); // 6 kept + 1 summary
        assert!(messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("auto-compacted"));
    }

    #[test]
    fn auto_compact_noop_when_under_threshold() {
        let mut messages: Vec<serde_json::Value> = (0..4)
            .map(|i| serde_json::json!({"role": "user", "content": format!("msg {i}")}))
            .collect();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let compacted = runtime.block_on(auto_compact_messages(
            &mut messages,
            &AutoCompactConfig {
                compact_strategy: CompactStrategy::Truncate,
                keep_last: 6,
                ..Default::default()
            },
            None,
        ));
        assert!(compacted.unwrap().is_none());
        assert_eq!(messages.len(), 4);
    }

    #[test]
    fn estimate_message_tokens_basic() {
        let messages = vec![
            serde_json::json!({"role": "user", "content": "a".repeat(400)}),
            serde_json::json!({"role": "assistant", "content": "b".repeat(400)}),
        ];
        let tokens = estimate_message_tokens(&messages);
        assert_eq!(tokens, 200); // 800 chars / 4
    }

    // ── Artifact dedup and microcompaction tests ─────────────────────

    #[test]
    fn dedup_artifacts_removes_duplicates() {
        let mut artifacts = vec![
            ArtifactRecord {
                id: "a1".to_string(),
                kind: "test".to_string(),
                text: Some("duplicate content".to_string()),
                ..Default::default()
            },
            ArtifactRecord {
                id: "a2".to_string(),
                kind: "test".to_string(),
                text: Some("duplicate content".to_string()),
                ..Default::default()
            },
            ArtifactRecord {
                id: "a3".to_string(),
                kind: "test".to_string(),
                text: Some("unique content".to_string()),
                ..Default::default()
            },
        ];
        dedup_artifacts(&mut artifacts);
        assert_eq!(artifacts.len(), 2);
    }

    #[test]
    fn microcompact_artifact_snips_oversized() {
        let mut artifact = ArtifactRecord {
            id: "a1".to_string(),
            kind: "test".to_string(),
            text: Some("x".repeat(10_000)),
            estimated_tokens: Some(2_500),
            ..Default::default()
        };
        microcompact_artifact(&mut artifact, 500);
        assert!(artifact.text.as_ref().unwrap().len() < 5_000);
        assert_eq!(artifact.estimated_tokens, Some(500));
    }

    // ── Tool argument constraint tests ───────────────────────────────

    #[test]
    fn arg_constraint_allows_matching_pattern() {
        let policy = CapabilityPolicy {
            tool_arg_constraints: vec![ToolArgConstraint {
                tool: "exec".to_string(),
                arg_patterns: vec!["cargo *".to_string()],
            }],
            ..Default::default()
        };
        let result = enforce_tool_arg_constraints(
            &policy,
            "exec",
            &serde_json::json!({"command": "cargo test"}),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn arg_constraint_rejects_non_matching_pattern() {
        let policy = CapabilityPolicy {
            tool_arg_constraints: vec![ToolArgConstraint {
                tool: "exec".to_string(),
                arg_patterns: vec!["cargo *".to_string()],
            }],
            ..Default::default()
        };
        let result = enforce_tool_arg_constraints(
            &policy,
            "exec",
            &serde_json::json!({"command": "rm -rf /"}),
        );
        assert!(result.is_err());
    }

    #[test]
    fn arg_constraint_ignores_unmatched_tool() {
        let policy = CapabilityPolicy {
            tool_arg_constraints: vec![ToolArgConstraint {
                tool: "exec".to_string(),
                arg_patterns: vec!["cargo *".to_string()],
            }],
            ..Default::default()
        };
        let result = enforce_tool_arg_constraints(
            &policy,
            "read_file",
            &serde_json::json!({"path": "/etc/passwd"}),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn microcompact_handles_multibyte_utf8() {
        // Emoji are 4 bytes each — slicing at arbitrary byte offsets would panic
        let emoji_output = "🔥".repeat(500); // 2000 bytes, 500 chars
        let result = microcompact_tool_output(&emoji_output, 400);
        // Should not panic and should contain the snip marker
        assert!(result.contains("snipped"));

        // Mixed ASCII + multi-byte
        let mixed = format!("{}{}{}", "a".repeat(300), "é".repeat(500), "b".repeat(300));
        let result2 = microcompact_tool_output(&mixed, 400);
        assert!(result2.contains("snipped"));

        // CJK characters (3 bytes each)
        let cjk = "中文".repeat(500);
        let result3 = microcompact_tool_output(&cjk, 400);
        assert!(result3.contains("snipped"));
    }
}
