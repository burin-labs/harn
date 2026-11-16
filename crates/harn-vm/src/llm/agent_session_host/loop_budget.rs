//! A read-only projection of the budget measured by the Harn loop.
//!
//! This store never advances a counter or grants another call. The loop
//! publishes at its call and terminal boundaries; callers read that same value.

use harn_builtin_macros::harn_builtin;
use serde::{Deserialize, Serialize};

use super::{json_to_vm, vm_to_json, with_session};
use crate::value::{VmError, VmValue};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LoopBudgetSnapshot {
    schema: String,
    iteration: u64,
    remaining: Option<u64>,
    max_iterations: Option<u64>,
    reserve: CallReserve,
    grants_remaining: CallGrants,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CallGrants {
    verification: u64,
    recovery: u64,
    terminal_report: u64,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CallReserve {
    verification: u64,
    recovery: u64,
    terminal_report: u64,
    total: u64,
}

/// Only the loop writes this projection; the public capability is read-only.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_set_loop_budget(session_id: string, snapshot: dict) -> nil",
    category = "agent.host",
    runtime_only = true
)]
fn set_loop_budget(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let id = args.first().map(VmValue::display).unwrap_or_default();
    let raw = args.get(1).ok_or_else(|| {
        VmError::Runtime("agent loop budget publication requires a snapshot".to_string())
    })?;
    let snapshot: LoopBudgetSnapshot =
        serde_json::from_value(vm_to_json(raw)).map_err(|error| {
            VmError::Runtime(format!("invalid agent loop budget snapshot: {error}"))
        })?;
    if snapshot.schema != "harn.agent_loop_budget_snapshot.v1" {
        return Err(VmError::Runtime(
            "unsupported agent loop budget snapshot schema".to_string(),
        ));
    }
    with_session(&id, "agent loop budget publication", |session| {
        session.loop_budget = Some(snapshot);
        Ok(VmValue::Nil)
    })
}

#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_loop_budget(session_id: string) -> {schema: string, iteration: int, remaining: int?, max_iterations: int?, reserve: {verification: int, recovery: int, terminal_report: int, total: int}, grants_remaining: {verification: int, recovery: int, terminal_report: int}}?",
    category = "agent.host",
    runtime_only = true
)]
fn loop_budget(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let id = args.first().map(VmValue::display).unwrap_or_default();
    with_session(
        &id,
        "agent loop budget observation",
        |session| match &session.loop_budget {
            Some(snapshot) => Ok(json_to_vm(&serde_json::to_value(snapshot).map_err(
                |error| VmError::Runtime(format!("agent loop budget projection: {error}")),
            )?)),
            None => Ok(VmValue::Nil),
        },
    )
}

pub(super) fn register_loop_budget_primitives(vm: &mut crate::vm::Vm) {
    harn_vm_core::builtin_support::register_module_builtins(vm, module_path!());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_measured_zero_and_other_sessions_are_distinct() {
        crate::llm::agent_session_host::reset_agent_session_host_state();
        for id in ["budget-root", "budget-child"] {
            crate::llm::agent_session_host::seed_host_session_provider_model(id, "mock", "mock");
        }
        let root = VmValue::String("budget-root".into());
        let child = VmValue::String("budget-child".into());
        assert!(matches!(
            loop_budget(&[root.clone()], &mut String::new()),
            Ok(VmValue::Nil)
        ));
        let snapshot = serde_json::json!({
            "schema": "harn.agent_loop_budget_snapshot.v1",
            "iteration": 2, "remaining": 0, "max_iterations": 2,
            "reserve": {"verification": 2, "recovery": 1, "terminal_report": 1, "total": 4},
            "grants_remaining": {"verification": 1, "recovery": 1, "terminal_report": 0}
        });
        set_loop_budget(&[root.clone(), json_to_vm(&snapshot)], &mut String::new()).unwrap();
        let observed = loop_budget(&[root.clone()], &mut String::new()).unwrap();
        assert_eq!(vm_to_json(&observed), snapshot);
        assert!(matches!(
            loop_budget(&[child], &mut String::new()),
            Ok(VmValue::Nil)
        ));
        let mut invalid = snapshot;
        invalid["remaining"] = serde_json::json!(-1);
        assert!(
            set_loop_budget(&[root.clone(), json_to_vm(&invalid)], &mut String::new()).is_err()
        );
        assert_eq!(
            vm_to_json(&loop_budget(&[root], &mut String::new()).unwrap())["remaining"],
            0
        );
        assert!(loop_budget(
            &[VmValue::String("unknown-session".into())],
            &mut String::new()
        )
        .is_err());
    }
}
