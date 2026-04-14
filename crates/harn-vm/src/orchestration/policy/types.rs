//! Policy type definitions — shapes used to describe agent capability
//! ceilings, turn/model/transcript policies, and the per-tool argument
//! constraint machinery. Everything here is plain data (+ a single
//! helper, `enforce_tool_arg_constraints`, that operates on it).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::super::glob_match;
use super::reject_policy;
use crate::tool_annotations::ToolAnnotations;
use crate::value::{VmError, VmValue};

/// Extended policy that supports argument-level constraints.
///
/// `arg_key` names the argument whose string value must match one of
/// `arg_patterns`. It is the self-describing form and should be set
/// explicitly by the policy author. When absent, the enforcer falls
/// back to `tool_annotations[tool].arg_schema.path_params`. If neither is populated,
/// the constraint is skipped with a structured `log_warn` — the VM is
/// intentionally domain-agnostic and does not guess argument semantics
/// by name (no "path"/"file"/"command"/... fallback list).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ToolArgConstraint {
    /// Tool name to constrain (glob-matched against dispatched tool names).
    pub tool: String,
    /// Glob patterns that the resolved argument value must match.
    /// If empty, no argument constraint is applied.
    pub arg_patterns: Vec<String>,
    /// Optional argument key whose string value is the constraint target.
    /// When present, overrides any metadata-derived key.
    #[serde(default)]
    pub arg_key: Option<String>,
}

/// Check if a tool call satisfies argument constraints in the policy.
///
/// Resolution order for which argument value to match:
/// 1. `constraint.arg_key` if set (explicit, self-describing).
/// 2. `policy.tool_annotations[tool].arg_schema.path_params` (first key
///    that yields a string value in the args object).
/// 3. No candidate — `log_warn` and skip the constraint. The VM refuses
///    to guess which argument holds the domain-relevant value.
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

        // Never guess which arg the constraint targets by common names —
        // that's a pipeline-level concern, not a VM concern.
        let declared_keys: Vec<String> = if let Some(key) = constraint.arg_key.as_ref() {
            vec![key.clone()]
        } else {
            policy
                .tool_annotations
                .get(tool_name)
                .map(|a| a.arg_schema.path_params.clone())
                .unwrap_or_default()
        };

        let (arg_key, arg_value): (String, Option<String>) = if let Some(obj) = args.as_object() {
            if declared_keys.is_empty() {
                // Permissive by design: missing annotations warn instead of
                // blocking, so a misconfigured policy can't silently wedge work.
                crate::events::log_warn(
                    "policy.constraint_unresolved",
                    &format!(
                        "tool_arg_constraint for tool '{}' has no arg_key and tool_annotations.arg_schema.path_params is empty; skipping (policy author should declare arg_key on the constraint or path_params in the tool's annotations)",
                        tool_name
                    ),
                );
                continue;
            }
            let mut found: (String, Option<String>) = (declared_keys[0].clone(), None);
            for param in &declared_keys {
                if let Some(value) = obj.get(param).and_then(|v| v.as_str()) {
                    found = (param.clone(), Some(value.to_string()));
                    break;
                }
            }
            found
        } else {
            ("value".to_string(), args.as_str().map(|s| s.to_string()))
        };

        // Absent arg ≠ rejection — constraint simply does not apply.
        let Some(candidate) = arg_value else {
            continue;
        };
        let matches = constraint
            .arg_patterns
            .iter()
            .any(|pattern| glob_match(pattern, &candidate));
        if !matches {
            return reject_policy(format!(
                "tool '{tool_name}' {arg_key} '{candidate}' does not match allowed patterns: {:?}. \
                 Only the {arg_key} argument is checked against this allow-list — other argument \
                 values are not.",
                constraint.arg_patterns
            ));
        }
    }
    Ok(())
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
    /// Per-tool annotations (kind, arg schema, capabilities, side-effect
    /// level). Pipelines own the registry; the VM reads it.
    #[serde(default)]
    pub tool_annotations: BTreeMap<String, ToolAnnotations>,
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

        let mut tool_arg_constraints = self.tool_arg_constraints.clone();
        tool_arg_constraints.extend(requested.tool_arg_constraints.clone());

        let tool_annotations = tools
            .iter()
            .filter_map(|tool| {
                requested
                    .tool_annotations
                    .get(tool)
                    .or_else(|| self.tool_annotations.get(tool))
                    .cloned()
                    .map(|annotations| (tool.clone(), annotations))
            })
            .collect();

        Ok(CapabilityPolicy {
            tools,
            capabilities,
            workspace_roots,
            side_effect_level,
            recursion_limit,
            tool_arg_constraints,
            tool_annotations,
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TurnPolicy {
    /// When true, text-only responses in a tool-capable stage are treated as
    /// invalid unless they switch phase / finish the stage. This keeps action
    /// stages moving instead of drifting into narration.
    pub require_action_or_yield: bool,
    /// When false, workflow-owned action stages should hand control back via
    /// successful tool calls instead of advertising an additional done
    /// sentinel pathway in corrective nudges.
    #[serde(default = "default_true")]
    pub allow_done_sentinel: bool,
    /// Optional visible prose budget for a single assistant turn. When the
    /// assistant exceeds it, the recorded transcript keeps only a shortened
    /// version and the next corrective nudge reminds the model to stay brief.
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

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ModelPolicy {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub model_tier: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<i64>,
    /// Maximum agent_loop iterations for this stage. Overrides the default 16.
    pub max_iterations: Option<usize>,
    /// Maximum consecutive text-only (no tool call) responses before declaring stuck.
    pub max_nudges: Option<usize>,
    /// Custom nudge message injected when the model produces text without tool calls.
    /// If omitted, the VM uses a generic "Continue — use a tool call" message.
    pub nudge: Option<String>,
    /// Few-shot tool-call examples injected into the tool contract prompt,
    /// shown before the tool schema listing. Pipelines provide these —
    /// the VM has no hardcoded tool names.
    pub tool_examples: Option<String>,
    /// Optional Harn closure called after each tool-calling turn.
    /// Receives turn metadata; returns either a string user message to inject,
    /// a bool stop flag, or a dict like {message, stop}.
    /// Wrapped in EqIgnored so it doesn't affect PartialEq derivation.
    #[serde(skip)]
    pub post_turn_callback: Option<EqIgnored<VmValue>>,
    /// When set, the stage stops after any tool-calling turn whose successful
    /// results include one of these tool names. This is useful for
    /// workflow-owned verify loops where a productive write turn should hand
    /// control back to verification immediately.
    pub stop_after_successful_tools: Option<Vec<String>>,
    /// When set, the stage is reported as failed unless at least one of these
    /// tool names succeeds during the interaction. Pipelines use this to
    /// assert a stage cannot quietly finish without running a specific tool.
    pub require_successful_tools: Option<Vec<String>>,
    /// Turn-shape constraints for action stages.
    pub turn_policy: Option<TurnPolicy>,
}

/// Wrapper that always compares equal, allowing non-Eq types in derived PartialEq structs.
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
pub struct TranscriptPolicy {
    pub mode: Option<String>,
    pub visibility: Option<String>,
    pub summarize: bool,
    pub compact: bool,
    pub keep_last: Option<usize>,
    /// Enable per-turn auto-compaction within agent loops.
    pub auto_compact: bool,
    /// Token threshold for tier-1 compaction.
    pub compact_threshold: Option<usize>,
    /// Max chars per tool result before compression.
    pub tool_output_max_chars: Option<usize>,
    /// Tier-1 compaction strategy name (e.g., "observation_mask", "llm").
    pub compact_strategy: Option<String>,
    /// Token threshold for tier-2 aggressive compaction.
    pub hard_limit_tokens: Option<usize>,
    /// Tier-2 compaction strategy name.
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
    /// Initial backoff duration in milliseconds between retry attempts.
    /// When `None`, retries proceed without delay.
    #[serde(default)]
    pub backoff_ms: Option<u64>,
    /// Multiplier applied to `backoff_ms` after each retry attempt.
    /// Defaults to 2.0 when `backoff_ms` is set and this field is `None`.
    #[serde(default)]
    pub backoff_multiplier: Option<f64>,
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
