//! Deterministic pre-approval tool-deny seam.
//!
//! An embedder can register a single deterministic precheck closure that the
//! agent-tool dispatch boundary consults BEFORE it emits any
//! `session/request_permission` prompt (or runs the dynamic-permission /
//! approval-policy gates). A precheck that refuses a call short-circuits it
//! straight to the model-facing denial, so a command the embedder has already
//! decided to block never asks the human to approve it first.
//!
//! The seam is generic over tools: the closure receives `{tool, arguments,
//! session_id}` and returns `nil` to allow or a deny descriptor carrying the
//! audience-split refusal (model cue, machine reason, human summary). It is
//! deliberately fail-OPEN — an unset precheck, or a closure that returns a
//! shape the boundary cannot read as a deny, leaves dispatch byte-identical to
//! the pre-seam behavior.

use std::cell::RefCell;
use std::sync::Arc;

use crate::value::{VmClosure, VmError, VmValue};

thread_local! {
    static TOOL_PRECHECK_STACK: RefCell<Vec<Arc<VmClosure>>> = const { RefCell::new(Vec::new()) };
    /// Re-entrancy depth. A precheck closure may itself dispatch tools; those
    /// nested dispatches must NOT recurse back into the precheck (which would
    /// loop forever), so the seam is inert while a precheck is on the stack.
    static TOOL_PRECHECK_DEPTH: RefCell<usize> = const { RefCell::new(0) };
}

/// Model-, machine-, and human-facing renderings of a precheck refusal. The
/// owner normalizes the closure's return into this typed shape once, at the
/// boundary, so the dispatch site consumes a guaranteed contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolPrecheckDenial {
    /// Model-facing text — becomes the denied `tool_result` reason.
    pub model_cue: String,
    /// Stable, secret-free machine reason for audit records, when supplied.
    pub machine_reason: Option<String>,
    /// One plain sentence for a human approval/denial surface, when supplied.
    pub human_summary: Option<String>,
    /// Whether re-issuing the identical call could ever succeed. Prechecks
    /// default to terminal (`false`); a closure may opt into a recoverable
    /// denial for an argument-scoped refusal.
    pub retryable: bool,
}

const DEFAULT_PRECHECK_CUE: &str = "command denied by policy";

pub fn push_tool_precheck(precheck: Arc<VmClosure>) {
    TOOL_PRECHECK_STACK.with(|stack| stack.borrow_mut().push(precheck));
}

pub fn pop_tool_precheck() {
    TOOL_PRECHECK_STACK.with(|stack| {
        stack.borrow_mut().pop();
    });
}

pub fn clear_tool_prechecks() {
    TOOL_PRECHECK_STACK.with(|stack| stack.borrow_mut().clear());
    TOOL_PRECHECK_DEPTH.with(|depth| *depth.borrow_mut() = 0);
}

/// The active precheck, if any. `None` when no embedder registered one — the
/// dispatch fast path skips the seam entirely in that case.
pub fn current_tool_precheck() -> Option<Arc<VmClosure>> {
    TOOL_PRECHECK_STACK.with(|stack| stack.borrow().last().cloned())
}

/// Whether a precheck is installed. Used by the dispatch fast-path gate to
/// keep the no-precheck path a provable no-op.
pub fn tool_precheck_active() -> bool {
    TOOL_PRECHECK_STACK.with(|stack| !stack.borrow().is_empty())
}

/// Per-task ambient-scope swap of the precheck stack. Mirrors the
/// command-policy stack swap so a spawned worker carries its own precheck
/// scope across `.await` without leaking into cooperatively-scheduled
/// siblings. Intentionally `pub(crate)` — only the ambient combinator moves
/// whole stacks.
pub(crate) fn swap_tool_precheck_stack(next: Vec<Arc<VmClosure>>) -> Vec<Arc<VmClosure>> {
    TOOL_PRECHECK_STACK.with(|stack| std::mem::replace(&mut *stack.borrow_mut(), next))
}

pub(crate) fn swap_tool_precheck_depth(next: usize) -> usize {
    TOOL_PRECHECK_DEPTH.with(|depth| std::mem::replace(&mut *depth.borrow_mut(), next))
}

/// Extract a precheck closure from an agent-loop `tool_precheck` option value.
/// The value is the predicate closure directly (or `nil` for none); any other
/// shape is a wiring error surfaced at install time.
pub fn parse_tool_precheck_value(
    value: Option<&VmValue>,
    label: &str,
) -> Result<Option<Arc<VmClosure>>, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Closure(closure)) => Ok(Some(closure.clone())),
        Some(other) => Err(VmError::Runtime(format!(
            "{label} must be a closure, got {}",
            other.type_name()
        ))),
    }
}

struct DepthGuard;

impl Drop for DepthGuard {
    fn drop(&mut self) {
        TOOL_PRECHECK_DEPTH.with(|depth| {
            let mut depth = depth.borrow_mut();
            *depth = depth.saturating_sub(1);
        });
    }
}

/// Consult the active precheck for one tool call. Returns `Ok(None)` when there
/// is no precheck, when the seam is re-entrant (a precheck closure's own tool
/// calls bypass the seam), or when the closure allows the call. Returns
/// `Ok(Some(_))` with the audience-split refusal when the closure denies.
pub async fn run_tool_precheck(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    tool_name: &str,
    tool_args: &serde_json::Value,
    session_id: &str,
) -> Result<Option<ToolPrecheckDenial>, VmError> {
    let Some(precheck) = current_tool_precheck() else {
        return Ok(None);
    };
    if TOOL_PRECHECK_DEPTH.with(|depth| *depth.borrow()) > 0 {
        return Ok(None);
    }
    let Some(mut vm) = ctx.map(crate::vm::AsyncBuiltinCtx::child_vm) else {
        // No VM context to run the closure in (e.g. a non-agent host call).
        // Fail open: leave dispatch unchanged rather than blocking blindly.
        return Ok(None);
    };
    let payload = serde_json::json!({
        "tool": tool_name,
        "arguments": tool_args,
        "session_id": session_id,
    });
    let arg = crate::stdlib::json_to_vm_value(&payload);
    TOOL_PRECHECK_DEPTH.with(|depth| *depth.borrow_mut() += 1);
    let _guard = DepthGuard;
    let verdict = vm.call_closure_pub(&precheck, &[arg]).await?;
    Ok(parse_precheck_verdict(verdict))
}

/// Normalize a precheck closure's return into a typed verdict. Fail-open: any
/// shape that is not recognizably a deny is treated as "allow" (`None`), so a
/// buggy or permissive precheck can never harden dispatch beyond the
/// pre-seam behavior.
fn parse_precheck_verdict(value: VmValue) -> Option<ToolPrecheckDenial> {
    match value {
        VmValue::Nil | VmValue::Bool(true) => None,
        VmValue::Bool(false) => Some(default_denial()),
        VmValue::String(text) => Some(ToolPrecheckDenial {
            model_cue: text.to_string(),
            machine_reason: None,
            human_summary: None,
            retryable: false,
        }),
        VmValue::Dict(map) => {
            // An explicit allow verdict short-circuits to None.
            if truthy(map.get("allow"))
                || map.get("decision").map(|v| v.display()).as_deref() == Some("allow")
            {
                return None;
            }
            // A `{deny: ...}` wrapper carries either a bare model-cue string or
            // a nested descriptor. Without a wrapper, the map itself is the
            // descriptor when it names a model cue.
            match map.get("deny") {
                Some(VmValue::String(text)) => Some(ToolPrecheckDenial {
                    model_cue: text.to_string(),
                    machine_reason: None,
                    human_summary: None,
                    retryable: false,
                }),
                Some(VmValue::Dict(inner)) => Some(denial_from_descriptor(inner)),
                Some(VmValue::Bool(true)) => Some(denial_from_descriptor(&map)),
                _ => {
                    // No `deny` key: treat the map as the descriptor only when
                    // it actually names a refusal (a model cue / reason).
                    if map.get("model_cue").is_some() || map.get("reason").is_some() {
                        Some(denial_from_descriptor(&map))
                    } else {
                        None
                    }
                }
            }
        }
        _ => None,
    }
}

fn denial_from_descriptor(map: &crate::value::DictMap) -> ToolPrecheckDenial {
    let model_cue = string_field(map, &["model_cue", "reason", "message"])
        .unwrap_or_else(|| DEFAULT_PRECHECK_CUE.to_string());
    ToolPrecheckDenial {
        model_cue,
        machine_reason: string_field(map, &["machine_reason"]),
        human_summary: string_field(map, &["human_summary"]),
        retryable: truthy(map.get("retryable")),
    }
}

fn default_denial() -> ToolPrecheckDenial {
    ToolPrecheckDenial {
        model_cue: DEFAULT_PRECHECK_CUE.to_string(),
        machine_reason: None,
        human_summary: None,
        retryable: false,
    }
}

/// First present, non-empty string among `keys`.
fn string_field(map: &crate::value::DictMap, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| match map.get(*key) {
        Some(VmValue::String(text)) if !text.is_empty() => Some(text.to_string()),
        _ => None,
    })
}

fn truthy(value: Option<&VmValue>) -> bool {
    matches!(value, Some(VmValue::Bool(true)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nil_and_true_allow() {
        assert_eq!(parse_precheck_verdict(VmValue::Nil), None);
        assert_eq!(parse_precheck_verdict(VmValue::Bool(true)), None);
    }

    #[test]
    fn bare_string_is_model_cue() {
        let denial = parse_precheck_verdict(VmValue::string("blocked: use look")).unwrap();
        assert_eq!(denial.model_cue, "blocked: use look");
        assert_eq!(denial.machine_reason, None);
        assert_eq!(denial.human_summary, None);
        assert!(!denial.retryable);
    }

    #[test]
    fn descriptor_splits_audiences() {
        let mut map = crate::value::DictMap::new();
        map.insert(crate::value::intern_key("deny"), VmValue::Bool(true));
        map.insert(
            crate::value::intern_key("model_cue"),
            VmValue::string("command denied by policy pattern 'ls *'; use `look`"),
        );
        map.insert(
            crate::value::intern_key("machine_reason"),
            VmValue::string("pattern 'ls *'"),
        );
        map.insert(
            crate::value::intern_key("human_summary"),
            VmValue::string("Blocked by command policy."),
        );
        let denial = parse_precheck_verdict(VmValue::dict(map)).unwrap();
        assert_eq!(denial.machine_reason.as_deref(), Some("pattern 'ls *'"));
        assert_eq!(
            denial.human_summary.as_deref(),
            Some("Blocked by command policy.")
        );
        assert!(denial.model_cue.contains("use `look`"));
    }

    #[test]
    fn explicit_allow_dict_is_none() {
        let mut map = crate::value::DictMap::new();
        map.insert(crate::value::intern_key("allow"), VmValue::Bool(true));
        assert_eq!(parse_precheck_verdict(VmValue::dict(map)), None);
    }

    #[test]
    fn bare_dict_without_cue_fails_open() {
        let mut map = crate::value::DictMap::new();
        map.insert(crate::value::intern_key("unrelated"), VmValue::Int(1));
        assert_eq!(parse_precheck_verdict(VmValue::dict(map)), None);
    }
}
