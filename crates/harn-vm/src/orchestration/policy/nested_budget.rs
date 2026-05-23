//! Centralized nested-execution budget for capability policies.
//!
//! `CapabilityPolicy::recursion_limit` is treated as the *remaining*
//! child-execution depth, not a static maximum. Entering a child
//! execution consumes one slot off the parent's budget; the child
//! receives `Some(n - 1)` in its effective policy. When the parent is
//! already at `Some(0)`, the helper rejects the launch with a
//! categorized [`crate::value::ErrorCategory::BudgetExceeded`] error
//! that names the nested surface kind and the target label.
//!
//! All Harn-owned child execution surfaces — `agent_loop`,
//! `sub_agent_run`, `spawn_agent` workers, workflow stage agent runs,
//! and nested Harn invocations — route through [`enter_nested_execution_policy`]
//! so the budget is checked + decremented exactly once per logical
//! child execution, audited consistently, and the error surface is
//! uniform.

use std::collections::BTreeMap;
use std::rc::Rc;

use super::CapabilityPolicy;
use crate::events::log_info_meta;
use crate::orchestration::{current_execution_policy, pop_execution_policy, push_execution_policy};
use crate::value::{ErrorCategory, VmError, VmValue};

/// Options-dict key for the nesting surface kind, read by
/// [`enter_nested_execution_policy`] at agent_loop entry.
pub const NESTED_KIND_OPTION_KEY: &str = "_nested_kind";
/// Options-dict key for the nesting surface label, read by
/// [`enter_nested_execution_policy`] at agent_loop entry.
pub const NESTED_LABEL_OPTION_KEY: &str = "_nested_label";

/// Categorizes the kind of nested execution surface for audit and
/// error messaging. The Harn surfaces that decrement the budget pass
/// the matching variant so users can tell which call exhausted the
/// allowance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NestedExecutionKind {
    /// A direct `agent_loop` invocation (top-level or nested).
    AgentLoop,
    /// A `sub_agent_run` foreground execution.
    SubAgentRun,
    /// A `spawn_agent` background worker about to run an agent loop.
    SpawnAgent,
    /// A workflow stage that launches agent work.
    WorkflowStage,
    /// A workflow execution started from inside another execution
    /// (workflow-of-workflows / nested `workflow_execute`).
    NestedWorkflow,
    /// A nested Harn invocation from CLI/API (e.g., `harn run` inside
    /// a parent policy scope, or a bridge/host re-entry).
    NestedInvocation,
}

impl NestedExecutionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentLoop => "agent_loop",
            Self::SubAgentRun => "sub_agent_run",
            Self::SpawnAgent => "spawn_agent",
            Self::WorkflowStage => "workflow_stage",
            Self::NestedWorkflow => "nested_workflow",
            Self::NestedInvocation => "nested_invocation",
        }
    }

    /// Parse a kind string from an options dict; falls back to
    /// [`Self::AgentLoop`] when the value is missing or unrecognized.
    pub fn parse_or_default(value: Option<&str>) -> Self {
        match value {
            Some("agent_loop") => Self::AgentLoop,
            Some("sub_agent_run") => Self::SubAgentRun,
            Some("spawn_agent") => Self::SpawnAgent,
            Some("workflow_stage") => Self::WorkflowStage,
            Some("nested_workflow") => Self::NestedWorkflow,
            Some("nested_invocation") => Self::NestedInvocation,
            _ => Self::AgentLoop,
        }
    }
}

/// Outcome of deriving a child execution policy. The guard pops the
/// pushed policy on drop; `parent_limit` / `child_limit` are preserved
/// for trace metadata.
#[derive(Debug)]
pub struct NestedExecutionGuard {
    pushed: bool,
    /// Parent's `recursion_limit` at the time of the descent. `None`
    /// means there was no Harn-side budget on the active stack.
    pub parent_limit: Option<usize>,
    /// `recursion_limit` that the child execution will observe.
    pub child_limit: Option<usize>,
    pub kind: NestedExecutionKind,
    pub label: String,
}

impl Drop for NestedExecutionGuard {
    fn drop(&mut self) {
        if self.pushed {
            pop_execution_policy();
        }
    }
}

/// Enter a child execution: validate the parent's recursion budget,
/// decrement once for this descent, and push a *budget-carrier* policy
/// onto the thread-local execution policy stack. The guard pops it on
/// drop. The carrier intentionally carries only `recursion_limit` —
/// tool, capability, and side-effect ceilings continue to flow through
/// the per-tool-dispatch policy guard so an agent's own `llm_call`
/// turns are not gated by its policy (which typically scopes tools,
/// not the agent's infrastructure). Per-tool-dispatch intersections
/// preserve the decremented budget because `CapabilityPolicy::intersect`
/// takes the `min` of `recursion_limit` across both sides.
pub fn enter_nested_execution_policy(
    requested: Option<CapabilityPolicy>,
    kind: NestedExecutionKind,
    label: &str,
) -> Result<NestedExecutionGuard, VmError> {
    let parent_limit = current_execution_policy().and_then(|p| p.recursion_limit);

    if matches!(parent_limit, Some(0)) {
        emit_descent_event(kind, label, parent_limit, None, true);
        return Err(nested_budget_exhausted(kind, label));
    }

    let requested_limit = requested.as_ref().and_then(|p| p.recursion_limit);
    let decremented_parent = parent_limit.map(|n| n - 1);
    let child_limit = match (decremented_parent, requested_limit) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };

    emit_descent_event(kind, label, parent_limit, child_limit, false);

    let pushed = if let Some(limit) = child_limit {
        push_execution_policy(CapabilityPolicy {
            recursion_limit: Some(limit),
            ..Default::default()
        });
        true
    } else {
        false
    };

    Ok(NestedExecutionGuard {
        pushed,
        parent_limit,
        child_limit,
        kind,
        label: label.to_string(),
    })
}

/// Tag an `agent_loop` options dict with the nested-execution kind and
/// label so [`enter_nested_execution_policy`] picks up the right
/// surface attribution at session init. Call sites that build options
/// for downstream agent_loop invocations (sub_agent_run, workflow
/// stages, spawn_agent worker setup) use this rather than rewriting
/// the dict-insert pattern.
pub fn annotate_nested_execution_options(
    options: &mut BTreeMap<String, VmValue>,
    kind: NestedExecutionKind,
    label: &str,
) {
    options.insert(
        NESTED_KIND_OPTION_KEY.to_string(),
        VmValue::String(Rc::from(kind.as_str().to_string())),
    );
    options.insert(
        NESTED_LABEL_OPTION_KEY.to_string(),
        VmValue::String(Rc::from(label.to_string())),
    );
}

fn nested_budget_exhausted(kind: NestedExecutionKind, label: &str) -> VmError {
    let label = if label.is_empty() { "<unnamed>" } else { label };
    VmError::CategorizedError {
        message: format!(
            "nested execution budget exhausted before {}: {}",
            kind.as_str(),
            label
        ),
        category: ErrorCategory::BudgetExceeded,
    }
}

fn emit_descent_event(
    kind: NestedExecutionKind,
    label: &str,
    parent_limit: Option<usize>,
    child_limit: Option<usize>,
    rejected: bool,
) {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "kind".to_string(),
        serde_json::Value::String(kind.as_str().to_string()),
    );
    metadata.insert(
        "label".to_string(),
        serde_json::Value::String(label.to_string()),
    );
    metadata.insert(
        "parent_recursion_limit".to_string(),
        recursion_limit_to_json(parent_limit),
    );
    metadata.insert(
        "child_recursion_limit".to_string(),
        recursion_limit_to_json(child_limit),
    );
    metadata.insert("rejected".to_string(), serde_json::Value::Bool(rejected));
    let message = if rejected {
        format!(
            "nested execution budget exhausted before {}: {}",
            kind.as_str(),
            label
        )
    } else {
        format!("nested execution descent into {}: {}", kind.as_str(), label)
    };
    log_info_meta("policy.nested_execution_descent", &message, metadata);
}

fn recursion_limit_to_json(value: Option<usize>) -> serde_json::Value {
    match value {
        Some(n) => serde_json::Value::Number(serde_json::Number::from(n)),
        None => serde_json::Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::clear_execution_policy_stacks;

    fn policy_with_limit(limit: Option<usize>) -> CapabilityPolicy {
        CapabilityPolicy {
            recursion_limit: limit,
            ..Default::default()
        }
    }

    #[test]
    fn none_parent_preserves_requested_limit() {
        clear_execution_policy_stacks();
        let requested = Some(policy_with_limit(Some(3)));
        let guard =
            enter_nested_execution_policy(requested, NestedExecutionKind::AgentLoop, "session-a")
                .unwrap();
        assert_eq!(guard.parent_limit, None);
        assert_eq!(guard.child_limit, Some(3));
        assert_eq!(current_execution_policy().unwrap().recursion_limit, Some(3));
        drop(guard);
        assert!(current_execution_policy().is_none());
    }

    #[test]
    fn some_one_allows_one_child_and_gives_child_zero() {
        clear_execution_policy_stacks();
        push_execution_policy(policy_with_limit(Some(1)));
        let guard =
            enter_nested_execution_policy(None, NestedExecutionKind::SubAgentRun, "child-1")
                .unwrap();
        assert_eq!(guard.parent_limit, Some(1));
        assert_eq!(guard.child_limit, Some(0));
        assert_eq!(current_execution_policy().unwrap().recursion_limit, Some(0));
        drop(guard);
        pop_execution_policy();
    }

    #[test]
    fn some_zero_rejects_with_budget_exceeded() {
        clear_execution_policy_stacks();
        push_execution_policy(policy_with_limit(Some(0)));
        let error =
            enter_nested_execution_policy(None, NestedExecutionKind::AgentLoop, "research-worker")
                .unwrap_err();
        match error {
            VmError::CategorizedError { message, category } => {
                assert_eq!(category, ErrorCategory::BudgetExceeded);
                assert!(
                    message.contains("agent_loop"),
                    "missing kind in message: {message}"
                );
                assert!(
                    message.contains("research-worker"),
                    "missing label in message: {message}"
                );
            }
            other => panic!("expected CategorizedError, got {other:?}"),
        }
        pop_execution_policy();
    }

    #[test]
    fn nested_chain_decrements_until_exhausted() {
        clear_execution_policy_stacks();
        let outer = enter_nested_execution_policy(
            Some(policy_with_limit(Some(2))),
            NestedExecutionKind::AgentLoop,
            "outer",
        )
        .unwrap();
        assert_eq!(outer.child_limit, Some(2));
        let middle =
            enter_nested_execution_policy(None, NestedExecutionKind::SubAgentRun, "middle")
                .unwrap();
        assert_eq!(middle.child_limit, Some(1));
        let inner =
            enter_nested_execution_policy(None, NestedExecutionKind::AgentLoop, "inner").unwrap();
        assert_eq!(inner.child_limit, Some(0));
        let exhausted =
            enter_nested_execution_policy(None, NestedExecutionKind::SubAgentRun, "innermost")
                .unwrap_err();
        assert!(matches!(
            exhausted,
            VmError::CategorizedError {
                category: ErrorCategory::BudgetExceeded,
                ..
            }
        ));
        drop(inner);
        drop(middle);
        drop(outer);
    }

    #[test]
    fn requested_limit_caps_below_parent() {
        clear_execution_policy_stacks();
        push_execution_policy(policy_with_limit(Some(8)));
        let guard = enter_nested_execution_policy(
            Some(policy_with_limit(Some(2))),
            NestedExecutionKind::WorkflowStage,
            "stage-1",
        )
        .unwrap();
        assert_eq!(guard.parent_limit, Some(8));
        // Decremented parent (7) intersected with requested (2) → 2.
        assert_eq!(guard.child_limit, Some(2));
        drop(guard);
        pop_execution_policy();
    }

    #[test]
    fn none_parent_and_none_requested_pushes_no_policy() {
        clear_execution_policy_stacks();
        let guard =
            enter_nested_execution_policy(None, NestedExecutionKind::NestedWorkflow, "wf-1")
                .unwrap();
        assert!(current_execution_policy().is_none());
        assert_eq!(guard.parent_limit, None);
        assert_eq!(guard.child_limit, None);
        drop(guard);
        assert!(current_execution_policy().is_none());
    }

    #[test]
    fn pushed_carrier_does_not_propagate_requested_tools_or_capabilities() {
        // Regression: the carrier intentionally exposes only the budget
        // to subsequent stack lookups. Tool, capability, and side-effect
        // ceilings flow through the per-tool-dispatch guard instead, so
        // the agent's own `llm_call` turn is not gated by a policy that
        // scopes the agent's tools to a read-only allowlist.
        clear_execution_policy_stacks();
        let requested = CapabilityPolicy {
            tools: vec!["read_only".to_string()],
            capabilities: std::collections::BTreeMap::from([(
                "workspace".to_string(),
                vec!["read_text".to_string()],
            )]),
            side_effect_level: Some("read_only".to_string()),
            recursion_limit: Some(4),
            ..Default::default()
        };
        let guard = enter_nested_execution_policy(
            Some(requested),
            NestedExecutionKind::AgentLoop,
            "session-x",
        )
        .unwrap();
        let pushed = current_execution_policy().unwrap();
        assert_eq!(pushed.recursion_limit, Some(4));
        assert!(pushed.tools.is_empty());
        assert!(pushed.capabilities.is_empty());
        assert!(pushed.side_effect_level.is_none());
        drop(guard);
    }

    #[test]
    fn workflow_stage_kind_observes_same_budget_semantics() {
        clear_execution_policy_stacks();
        push_execution_policy(policy_with_limit(Some(1)));
        // Workflow stage is just another nested surface — the budget
        // gate decrements identically and surfaces the stage label on
        // rejection so workflow authors can see which node tripped.
        let guard =
            enter_nested_execution_policy(None, NestedExecutionKind::WorkflowStage, "build_stage")
                .unwrap();
        assert_eq!(guard.child_limit, Some(0));
        // Next stage would try to nest under a zero-budget parent.
        let denied =
            enter_nested_execution_policy(None, NestedExecutionKind::WorkflowStage, "verify_stage")
                .unwrap_err();
        match denied {
            VmError::CategorizedError { message, category } => {
                assert_eq!(category, ErrorCategory::BudgetExceeded);
                assert!(message.contains("workflow_stage"));
                assert!(message.contains("verify_stage"));
            }
            other => panic!("expected CategorizedError, got {other:?}"),
        }
        drop(guard);
        pop_execution_policy();
    }

    #[test]
    fn annotate_nested_execution_options_writes_canonical_keys() {
        let mut options: BTreeMap<String, VmValue> = BTreeMap::new();
        annotate_nested_execution_options(
            &mut options,
            NestedExecutionKind::SubAgentRun,
            "research-worker",
        );
        match options.get(NESTED_KIND_OPTION_KEY).unwrap() {
            VmValue::String(text) => assert_eq!(text.as_ref(), "sub_agent_run"),
            _ => panic!("kind not stored as string"),
        }
        match options.get(NESTED_LABEL_OPTION_KEY).unwrap() {
            VmValue::String(text) => assert_eq!(text.as_ref(), "research-worker"),
            _ => panic!("label not stored as string"),
        }
    }
}
