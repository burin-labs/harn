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

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::thread::LocalKey;

use crate::value::{VmError, VmValue};

thread_local! {
    static MCP_CALL_BUDGET: RefCell<Option<u64>> = const { RefCell::new(None) };
    static MCP_CALL_COUNT: RefCell<u64> = const { RefCell::new(0) };
    static PG_QUERY_BUDGET: RefCell<Option<u64>> = const { RefCell::new(None) };
    static PG_QUERY_COUNT: RefCell<u64> = const { RefCell::new(0) };
}

/// Reset thread-local call-budget state. Call between test runs so a
/// guard that outlived an unwinding test cannot leak a ceiling.
pub(crate) fn reset_call_budget_state() {
    MCP_CALL_BUDGET.with(|b| *b.borrow_mut() = None);
    MCP_CALL_COUNT.with(|c| *c.borrow_mut() = 0);
    PG_QUERY_BUDGET.with(|b| *b.borrow_mut() = None);
    PG_QUERY_COUNT.with(|c| *c.borrow_mut() = 0);
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
    budget: &'static LocalKey<RefCell<Option<u64>>>,
    count: &'static LocalKey<RefCell<u64>>,
    kind: CallBudgetKind,
) -> Result<(), VmError> {
    let Some(max) = budget.with(|b| *b.borrow()) else {
        return Ok(());
    };
    let spent = count.with(|c| {
        let mut slot = c.borrow_mut();
        *slot = slot.saturating_add(1);
        *slot
    });
    if spent > max {
        return Err(budget_exceeded_error(kind, spent, max));
    }
    Ok(())
}

/// Build the structured error rendered as HTTP 429. The `category` field
/// routes it through `ErrorCategory::BudgetExceeded`; the `limit` field
/// names the dimension so adapters report `code: "budget_exceeded"` with
/// the precise `@budget(...)` field that fired.
fn budget_exceeded_error(kind: CallBudgetKind, spent: u64, max: u64) -> VmError {
    let mut dict = BTreeMap::new();
    dict.insert(
        "category".to_string(),
        VmValue::String(Rc::from("budget_exceeded")),
    );
    dict.insert("kind".to_string(), VmValue::String(Rc::from("terminal")));
    dict.insert(
        "reason".to_string(),
        VmValue::String(Rc::from("budget_exceeded")),
    );
    dict.insert(
        "limit".to_string(),
        VmValue::String(Rc::from(kind.limit_label())),
    );
    dict.insert("limit_value".to_string(), VmValue::Int(max as i64));
    dict.insert("spent".to_string(), VmValue::Int(spent as i64));
    dict.insert(
        "message".to_string(),
        VmValue::String(Rc::from(format!(
            "{} budget exceeded: this dispatch attempted {} of {} permitted {}",
            kind.limit_label(),
            spent,
            max,
            kind.noun(max != 1),
        ))),
    );
    VmError::Thrown(VmValue::Dict(Rc::new(dict)))
}

/// RAII guard for [`install_mcp_call_budget`]. Restores the prior MCP
/// call ceiling and count on drop.
#[must_use = "dropping the guard immediately restores the prior MCP call budget"]
pub struct McpCallBudgetGuard {
    previous_budget: Option<u64>,
    previous_count: u64,
}

impl Drop for McpCallBudgetGuard {
    fn drop(&mut self) {
        MCP_CALL_BUDGET.with(|b| *b.borrow_mut() = self.previous_budget);
        MCP_CALL_COUNT.with(|c| *c.borrow_mut() = self.previous_count);
    }
}

/// Pin the per-dispatch MCP tool-call ceiling at `max` for the lifetime
/// of the returned guard. Sourced from `@budget(mcp_calls: …)` on
/// `.harn` handlers in `harn-serve`; the `(max + 1)`-th call raises a
/// `BudgetExceeded`-categorised error adapters render as HTTP 429.
pub fn install_mcp_call_budget(max: u64) -> McpCallBudgetGuard {
    let previous_budget = MCP_CALL_BUDGET.with(|b| *b.borrow());
    let previous_count = MCP_CALL_COUNT.with(|c| *c.borrow());
    MCP_CALL_BUDGET.with(|b| *b.borrow_mut() = Some(max));
    MCP_CALL_COUNT.with(|c| *c.borrow_mut() = 0);
    McpCallBudgetGuard {
        previous_budget,
        previous_count,
    }
}

/// Charge one MCP tool call against the active `@budget(mcp_calls: …)`
/// ceiling, if any. Called once per logical `mcp.call` dispatch.
pub fn charge_mcp_call() -> Result<(), VmError> {
    charge(&MCP_CALL_BUDGET, &MCP_CALL_COUNT, CallBudgetKind::McpCalls)
}

/// RAII guard for [`install_pg_query_budget`]. Restores the prior
/// Postgres query ceiling and count on drop.
#[must_use = "dropping the guard immediately restores the prior Postgres query budget"]
pub struct PgQueryBudgetGuard {
    previous_budget: Option<u64>,
    previous_count: u64,
}

impl Drop for PgQueryBudgetGuard {
    fn drop(&mut self) {
        PG_QUERY_BUDGET.with(|b| *b.borrow_mut() = self.previous_budget);
        PG_QUERY_COUNT.with(|c| *c.borrow_mut() = self.previous_count);
    }
}

/// Pin the per-dispatch Postgres query ceiling at `max` for the lifetime
/// of the returned guard. Sourced from `@budget(pg_queries: …)` on
/// `.harn` handlers in `harn-serve`; the `(max + 1)`-th query raises a
/// `BudgetExceeded`-categorised error adapters render as HTTP 429.
pub fn install_pg_query_budget(max: u64) -> PgQueryBudgetGuard {
    let previous_budget = PG_QUERY_BUDGET.with(|b| *b.borrow());
    let previous_count = PG_QUERY_COUNT.with(|c| *c.borrow());
    PG_QUERY_BUDGET.with(|b| *b.borrow_mut() = Some(max));
    PG_QUERY_COUNT.with(|c| *c.borrow_mut() = 0);
    PgQueryBudgetGuard {
        previous_budget,
        previous_count,
    }
}

/// Charge one Postgres query against the active `@budget(pg_queries: …)`
/// ceiling, if any. Called once per `pg_query` / `pg_query_one` /
/// `pg_execute` statement (including mock-pool statements).
pub fn charge_pg_query() -> Result<(), VmError> {
    charge(&PG_QUERY_BUDGET, &PG_QUERY_COUNT, CallBudgetKind::PgQueries)
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
        // No guard installed → counters never advance.
        assert_eq!(MCP_CALL_COUNT.with(|c| *c.borrow()), 0);
        assert_eq!(PG_QUERY_COUNT.with(|c| *c.borrow()), 0);
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
        assert_eq!(MCP_CALL_COUNT.with(|c| *c.borrow()), 1);

        {
            // Nested dispatch installs a tighter ceiling and starts fresh.
            let _inner = install_mcp_call_budget(1);
            assert_eq!(MCP_CALL_COUNT.with(|c| *c.borrow()), 0);
            assert!(charge_mcp_call().is_ok());
            assert!(charge_mcp_call().is_err());
        }

        // Inner drop restores the outer ceiling and its accumulated count.
        assert_eq!(MCP_CALL_BUDGET.with(|b| *b.borrow()), Some(5));
        assert_eq!(MCP_CALL_COUNT.with(|c| *c.borrow()), 1);
        drop(outer);
        assert_eq!(MCP_CALL_BUDGET.with(|b| *b.borrow()), None);
        reset_call_budget_state();
    }
}
