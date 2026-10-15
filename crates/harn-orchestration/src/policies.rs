//! Workflow policies with callback values owned by the interpreter.

use harn_vm_core::VmValue;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TurnPolicy {
    /// Reject text-only responses in stages that require tool action or yield.
    pub require_action_or_yield: bool,
    /// Permit a separate done sentinel alongside successful tool completion.
    #[serde(default = "default_true")]
    pub allow_done_sentinel: bool,
    /// Shorten excess prose in the transcript and nudge the next turn.
    pub max_prose_chars: Option<usize>,
}

impl Default for TurnPolicy {
    fn default() -> Self {
        Self {
            require_action_or_yield: false,
            allow_done_sentinel: true,
            max_prose_chars: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeToolFallbackPolicy {
    Allow,
    AllowOnce,
    #[default]
    Reject,
}

impl NativeToolFallbackPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::AllowOnce => "allow_once",
            Self::Reject => "reject",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "allow" => Some(Self::Allow),
            "allow_once" => Some(Self::AllowOnce),
            "reject" => Some(Self::Reject),
            _ => None,
        }
    }
}

fn default_true() -> bool {
    true
}

/// Every outer clause is mandatory; an inner group accepts one successful tool.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum RequiredSuccessfulTool {
    Tool(String),
    AnyOf(Vec<String>),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ModelPolicy {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub model_tier: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<i64>,
    /// Static iteration cap; the adaptive budget's maximum takes precedence.
    pub max_iterations: Option<usize>,
    /// Adaptive `{mode, initial, max, extend_by}` budget, validated by
    /// std/agent/options and passed to agent_loop for extension/stop receipts.
    pub iteration_budget: Option<serde_json::Value>,
    pub max_nudges: Option<usize>,
    /// Corrective message for a turn without tool calls.
    pub nudge: Option<String>,
    /// Workflow-provided examples preceding the tool schema listing.
    pub tool_examples: Option<String>,
    /// Called after tool-calling turns with turn metadata. Returns a message,
    /// a stop flag, or `{message, stop}`. Excluded from serialization and equality.
    #[serde(skip)]
    pub post_turn_callback: Option<EqIgnored<VmValue>>,
    /// Hand back control after a turn successfully calls one of these tools.
    pub stop_after_successful_tools: Option<Vec<String>>,
    /// Fail the stage unless every clause is satisfied by successful calls.
    pub require_successful_tools: Option<Vec<RequiredSuccessfulTool>>,
    pub turn_policy: Option<TurnPolicy>,
    /// Native provider tools or text tool-call contract; std/workflow/options
    /// composes host defaults when this is absent.
    pub tool_format: Option<String>,
    /// Treatment of text tool calls emitted during a native-tool stage.
    pub native_tool_fallback: NativeToolFallbackPolicy,
    /// Validated by std/agent/stall_config; absent values inherit agent_loop.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stall_diagnostics: Option<serde_json::Value>,
}

/// Excludes runtime callback identity from policy equality.
#[derive(Clone, Debug, Default)]
pub struct EqIgnored<T>(pub T);

impl<T> PartialEq for EqIgnored<T> {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl<T> std::ops::Deref for EqIgnored<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AutoCompactPolicy {
    pub enabled: bool,
    pub token_threshold: Option<usize>,
    /// Maximum characters retained from a tool result before compression.
    pub tool_output_max_chars: Option<usize>,
    /// Tier-one strategy, such as observation_mask or llm.
    pub compact_strategy: Option<String>,
    /// Token threshold and strategy for tier-two compaction.
    pub hard_limit_tokens: Option<usize>,
    pub hard_limit_strategy: Option<String>,
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

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RetryPolicy {
    pub max_attempts: usize,
    pub verify: bool,
    pub repair: bool,
    /// Initial retry delay; absent values retry immediately.
    #[serde(default)]
    pub backoff_ms: Option<u64>,
    /// Defaults to 2.0 when backoff_ms is set.
    #[serde(default)]
    pub backoff_multiplier: Option<f64>,
    /// Thread verification findings into the next task. Interpreted by
    /// std/workflow/stage. Omitted when unset to preserve workflow graph digests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<FeedbackPolicy>,
    /// Runtime closure receiving retry context and returning the next prompt.
    /// The embedded stage loop invokes it; Rust carries the value unchanged.
    #[serde(skip)]
    pub repair_prompt_builder: Option<EqIgnored<VmValue>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum FeedbackPolicy {
    Enabled(bool),
    Bounded(FeedbackBounds),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct FeedbackBounds {
    pub max_chars: Option<usize>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct MapPolicy {
    pub items: Vec<serde_json::Value>,
    pub item_artifact_kind: Option<String>,
    pub output_kind: Option<String>,
    pub max_items: Option<usize>,
    pub max_concurrent: Option<usize>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_policy_round_trips_required_successful_tool_or_groups() {
        let value =
            serde_json::json!({"require_successful_tools": ["verify", ["edit", "scaffold"]]});
        let policy: ModelPolicy = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            policy.require_successful_tools,
            Some(vec![
                RequiredSuccessfulTool::Tool("verify".to_string()),
                RequiredSuccessfulTool::AnyOf(vec!["edit".to_string(), "scaffold".to_string()]),
            ])
        );
        assert_eq!(
            serde_json::to_value(policy)
                .unwrap()
                .get("require_successful_tools"),
            value.get("require_successful_tools")
        );
    }
}
