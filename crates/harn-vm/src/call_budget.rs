//! Per-dispatch ceilings on outbound *call counts* — MCP tool calls and
//! Postgres queries — mirroring the LLM cost/token budgets in
//! [`crate::llm::cost`]. A `.harn` handler exported through `harn-serve`
//! declares `@budget(mcp_calls: 20, pg_queries: 50)`; the dispatcher
//! installs the matching guards for the lifetime of the call. Each
//! charge increments a per-thread counter and, once the ceiling is
//! crossed, raises a structured `BudgetExceeded`-categorised error that
//! adapter codecs render as HTTP 429.
//!
//! Counters only advance while a budget is installed, so dispatches
//! without a `@budget` declaration pay nothing and never accumulate
//! cross-call state. Guards restore the prior ceiling and count on drop,
//! keeping nested dispatches (a handler that re-enters the dispatcher)
//! from leaking a tighter budget outward or a wider one back into a
//! finished inner scope.
//!
//! A ceiling and its running count travel together in one [`CallBudget`],
//! and the count lives behind an `Arc`. That is what makes the ceiling hold
//! when a dispatch fans out: `parallel each { mcp.call(..) }` charges the
//! SAME counter the dispatcher installed, even though each branch runs on a
//! different thread. A per-branch copy of the count would let every branch
//! spend the whole ceiling.

use crate::value::VmDictExt;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::LocalKey;

use crate::value::{VmError, VmValue};

/// One dispatch's ceiling plus the count spent against it.
///
/// Cloning shares the count. `AmbientExecutionScope` clones this into every
/// subtask, so a fan-out spends one budget rather than one budget per branch.
#[derive(Clone, Debug)]
pub(crate) struct CallBudget {
    max: u64,
    spent: Arc<AtomicU64>,
}

impl CallBudget {
    fn new(max: u64) -> Self {
        Self {
            max,
            spent: Arc::new(AtomicU64::new(0)),
        }
    }

    fn spent(&self) -> u64 {
        self.spent.load(Ordering::Relaxed)
    }
}

thread_local! {
    static MCP_CALL_BUDGET: RefCell<Option<CallBudget>> = const { RefCell::new(None) };
    static PG_QUERY_BUDGET: RefCell<Option<CallBudget>> = const { RefCell::new(None) };
}

/// Swap the MCP call budget. Paired with `AmbientExecutionScope`'s per-poll
/// swap.
pub(crate) fn swap_mcp_call_budget(next: Option<CallBudget>) -> Option<CallBudget> {
    MCP_CALL_BUDGET.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), next))
}

/// Swap the Postgres query budget. Paired with `AmbientExecutionScope`'s
/// per-poll swap.
pub(crate) fn swap_pg_query_budget(next: Option<CallBudget>) -> Option<CallBudget> {
    PG_QUERY_BUDGET.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), next))
}

/// Reset thread-local call-budget state. Call between test runs so a
/// guard that outlived an unwinding test cannot leak a ceiling.
pub(crate) fn reset_call_budget_state() {
    MCP_CALL_BUDGET.with(|b| *b.borrow_mut() = None);
    PG_QUERY_BUDGET.with(|b| *b.borrow_mut() = None);
}

/// The two call-count dimensions. Each names the `@budget(...)` field it
/// backs so the structured error carries the dimension that fired and
/// `harn-serve`'s `budget_category_from_error` can recover it from the
/// `limit` field without inspecting the message.
#[derive(Clone, Copy)]
enum CallBudgetKind {
    McpCalls,
    PgQueries,
}

impl CallBudgetKind {
    /// The `@budget(...)` field name, surfaced as the error's `limit`.
    fn limit_label(self) -> &'static str {
        match self {
            CallBudgetKind::McpCalls => "mcp_calls",
            CallBudgetKind::PgQueries => "pg_queries",
        }
    }

    /// Human-readable noun for the error message, pluralised to agree
    /// with the ceiling count.
    fn noun(self, plural: bool) -> &'static str {
        match (self, plural) {
            (CallBudgetKind::McpCalls, false) => "MCP call",
            (CallBudgetKind::McpCalls, true) => "MCP calls",
            (CallBudgetKind::PgQueries, false) => "Postgres query",
            (CallBudgetKind::PgQueries, true) => "Postgres queries",
        }
    }
}

/// Increment the counter behind `budget`/`count` and raise once the
/// ceiling is crossed. A `None` budget short-circuits — no install, no
/// charge. The counter only advances while a ceiling is present so
/// budget-free dispatches stay zero-cost.
fn charge(
    budget: &'static LocalKey<RefCell<Option<CallBudget>>>,
    kind: CallBudgetKind,
) -> Result<(), VmError> {
    let Some(budget) = budget.with(|b| b.borrow().clone()) else {
        return Ok(());
    };
    let spent = budget
        .spent
        .fetch_add(1, Ordering::Relaxed)
        .saturating_add(1);
    if spent > budget.max {
        return Err(budget_exceeded_error(kind, spent, budget.max));
    }
    Ok(())
}

/// Build the structured error rendered as HTTP 429. The `category` field
/// routes it through `ErrorCategory::BudgetExceeded`; the `limit` field
/// names the dimension so adapters report `code: "budget_exceeded"` with
/// the precise `@budget(...)` field that fired.
fn budget_exceeded_error(kind: CallBudgetKind, spent: u64, max: u64) -> VmError {
    let mut dict = BTreeMap::new();
    dict.put_str("category", "budget_exceeded");
    dict.put_str("kind", "terminal");
    dict.put_str("reason", "budget_exceeded");
    dict.put_str("limit", kind.limit_label());
    dict.insert("limit_value".to_string(), VmValue::Int(max as i64));
    dict.insert("spent".to_string(), VmValue::Int(spent as i64));
    dict.put_str(
        "message",
        format!(
            "{} budget exceeded: this dispatch attempted {} of {} permitted {}",
            kind.limit_label(),
            spent,
            max,
            kind.noun(max != 1),
        ),
    );
    VmError::Thrown(VmValue::dict(dict))
}

/// RAII guard for [`install_mcp_call_budget`]. Restores the prior MCP
/// call ceiling and count on drop.
#[must_use = "dropping the guard immediately restores the prior MCP call budget"]
pub struct McpCallBudgetGuard {
    previous: Option<CallBudget>,
}

impl Drop for McpCallBudgetGuard {
    fn drop(&mut self) {
        MCP_CALL_BUDGET.with(|b| *b.borrow_mut() = self.previous.take());
    }
}

/// Pin the per-dispatch MCP tool-call ceiling at `max` for the lifetime
/// of the returned guard. Sourced from `@budget(mcp_calls: …)` on
/// `.harn` handlers in `harn-serve`; the `(max + 1)`-th call raises a
/// `BudgetExceeded`-categorised error adapters render as HTTP 429.
pub fn install_mcp_call_budget(max: u64) -> McpCallBudgetGuard {
    McpCallBudgetGuard {
        previous: swap_mcp_call_budget(Some(CallBudget::new(max))),
    }
}

/// Charge one MCP tool call against the active `@budget(mcp_calls: …)`
/// ceiling, if any. Called once per logical `mcp.call` dispatch.
pub fn charge_mcp_call() -> Result<(), VmError> {
    charge(&MCP_CALL_BUDGET, CallBudgetKind::McpCalls)
}

/// The MCP calls charged against the active ceiling, or `None` when no
/// `@budget(mcp_calls: …)` is installed. Exists so a test can prove a
/// subtask's charges reach its parent's counter.
pub fn mcp_calls_spent() -> Option<u64> {
    MCP_CALL_BUDGET.with(|b| b.borrow().as_ref().map(CallBudget::spent))
}

/// RAII guard for [`install_pg_query_budget`]. Restores the prior
/// Postgres query ceiling and count on drop.
#[must_use = "dropping the guard immediately restores the prior Postgres query budget"]
pub struct PgQueryBudgetGuard {
    previous: Option<CallBudget>,
}

impl Drop for PgQueryBudgetGuard {
    fn drop(&mut self) {
        PG_QUERY_BUDGET.with(|b| *b.borrow_mut() = self.previous.take());
    }
}

/// Pin the per-dispatch Postgres query ceiling at `max` for the lifetime
/// of the returned guard. Sourced from `@budget(pg_queries: …)` on
/// `.harn` handlers in `harn-serve`; the `(max + 1)`-th query raises a
/// `BudgetExceeded`-categorised error adapters render as HTTP 429.
pub fn install_pg_query_budget(max: u64) -> PgQueryBudgetGuard {
    PgQueryBudgetGuard {
        previous: swap_pg_query_budget(Some(CallBudget::new(max))),
    }
}

/// Charge one Postgres query against the active `@budget(pg_queries: …)`
/// ceiling, if any. Called once per `pg_query` / `pg_query_one` /
/// `pg_execute` statement (including mock-pool statements).
pub fn charge_pg_query() -> Result<(), VmError> {
    charge(&PG_QUERY_BUDGET, CallBudgetKind::PgQueries)
}

/// The Postgres queries charged against the active ceiling, or `None` when no
/// `@budget(pg_queries: …)` is installed.
pub fn pg_queries_spent() -> Option<u64> {
    PG_QUERY_BUDGET.with(|b| b.borrow().as_ref().map(CallBudget::spent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{error_to_category, ErrorCategory};

    #[test]
    fn charge_is_noop_without_installed_budget() {
        reset_call_budget_state();
        for _ in 0..1000 {
            assert!(charge_mcp_call().is_ok());
            assert!(charge_pg_query().is_ok());
        }
        // No guard installed → nothing to advance.
        assert_eq!(mcp_calls_spent(), None);
        assert_eq!(pg_queries_spent(), None);
    }

    #[test]
    fn mcp_budget_admits_up_to_ceiling_then_rejects() {
        reset_call_budget_state();
        let _guard = install_mcp_call_budget(2);
        assert!(charge_mcp_call().is_ok());
        assert!(charge_mcp_call().is_ok());
        let third = charge_mcp_call();
        let err = third.expect_err("third call must exceed mcp_calls: 2");
        assert_eq!(error_to_category(&err), ErrorCategory::BudgetExceeded);
        match &err {
            VmError::Thrown(VmValue::Dict(d)) => {
                assert_eq!(
                    d.get("limit").map(|v| v.display()).as_deref(),
                    Some("mcp_calls")
                );
                assert_eq!(d.get("limit_value").and_then(VmValue::as_int), Some(2));
                assert_eq!(d.get("spent").and_then(VmValue::as_int), Some(3));
            }
            other => panic!("expected structured Thrown dict, got {other:?}"),
        }
        reset_call_budget_state();
    }

    #[test]
    fn pg_budget_message_pluralises_and_names_dimension() {
        reset_call_budget_state();
        let _guard = install_pg_query_budget(1);
        assert!(charge_pg_query().is_ok());
        let err = charge_pg_query().expect_err("second query must exceed pg_queries: 1");
        match &err {
            VmError::Thrown(VmValue::Dict(d)) => {
                let message = d.get("message").map(|v| v.display()).unwrap_or_default();
                assert!(
                    message.contains("pg_queries budget exceeded"),
                    "got: {message}"
                );
                assert!(message.contains("Postgres query"), "got: {message}");
            }
            other => panic!("expected structured Thrown dict, got {other:?}"),
        }
        reset_call_budget_state();
    }

    #[test]
    fn nested_guard_restores_outer_budget_and_count_on_drop() {
        reset_call_budget_state();
        let outer = install_mcp_call_budget(5);
        assert!(charge_mcp_call().is_ok());
        assert_eq!(mcp_calls_spent(), Some(1));

        {
            // Nested dispatch installs a tighter ceiling and starts fresh.
            let _inner = install_mcp_call_budget(1);
            assert_eq!(mcp_calls_spent(), Some(0));
            assert!(charge_mcp_call().is_ok());
            assert!(charge_mcp_call().is_err());
        }

        // Inner drop restores the outer ceiling and its accumulated count.
        assert_eq!(
            MCP_CALL_BUDGET.with(|b| b.borrow().as_ref().map(|b| b.max)),
            Some(5)
        );
        assert_eq!(mcp_calls_spent(), Some(1));
        drop(outer);
        assert_eq!(mcp_calls_spent(), None);
        reset_call_budget_state();
    }
}
