//! Captured budget owners. A new dispatch gets fresh accounting; cloned
//! ambient scopes share the ceiling and ledger across inline and worker fan-out.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use super::ObservedSessionUsage;

#[derive(Debug, Default)]
pub(super) struct CostLedger {
    pub max: Option<f64>,
    pub spent: f64,
    pub observed: ObservedSessionUsage,
}

#[derive(Debug, Default)]
pub(super) struct TokenLedger {
    pub max: Option<u64>,
    pub spent: u64,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LlmCostBudget(Arc<Mutex<CostLedger>>);

#[derive(Clone, Debug, Default)]
pub(crate) struct LlmTokenBudget(Arc<Mutex<TokenLedger>>);

/// A shared cost owner for host observations and out-of-band ceiling updates.
/// Cloning shares accounting with the installation and all inherited subtasks.
#[derive(Clone, Debug)]
pub struct LlmCostBudgetHandle(LlmCostBudget);

impl LlmCostBudgetHandle {
    pub fn total_cost(&self) -> f64 {
        self.0 .0.lock().unwrap_or_else(|e| e.into_inner()).spent
    }

    /// Re-arm this owner's ceiling, preserving accumulated spend and usage.
    pub fn set_ceiling(&self, max_cost_usd: Option<f64>) {
        self.0 .0.lock().unwrap_or_else(|e| e.into_inner()).max =
            normalize_cost_ceiling(max_cost_usd);
    }
}

impl PartialEq for LlmCostBudgetHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0 .0, &other.0 .0)
    }
}
impl Eq for LlmCostBudgetHandle {}

/// A shared token owner for out-of-band ceiling updates.
#[derive(Clone, Debug)]
pub struct LlmTokenBudgetHandle(LlmTokenBudget);

impl LlmTokenBudgetHandle {
    /// Re-arm this owner's ceiling without resetting accumulated tokens.
    pub fn set_ceiling(&self, max_tokens: Option<u64>) {
        self.0 .0.lock().unwrap_or_else(|e| e.into_inner()).max = max_tokens;
    }
}

impl PartialEq for LlmTokenBudgetHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0 .0, &other.0 .0)
    }
}
impl Eq for LlmTokenBudgetHandle {}

fn normalize_cost_ceiling(max_cost_usd: Option<f64>) -> Option<f64> {
    max_cost_usd.map(|max| max.max(0.0))
}

thread_local! {
    static LLM_COST_BUDGET: RefCell<Option<LlmCostBudget>> = const { RefCell::new(None) };
    static LLM_TOKEN_BUDGET: RefCell<Option<LlmTokenBudget>> = const { RefCell::new(None) };
}

pub(crate) fn swap_llm_cost_budget(next: Option<LlmCostBudget>) -> Option<LlmCostBudget> {
    LLM_COST_BUDGET.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), next))
}

pub(crate) fn swap_llm_token_budget(next: Option<LlmTokenBudget>) -> Option<LlmTokenBudget> {
    LLM_TOKEN_BUDGET.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), next))
}

pub(super) fn with_cost_budget<T>(f: impl FnOnce(&mut CostLedger) -> T) -> T {
    LLM_COST_BUDGET.with(|slot| {
        let mut slot = slot.borrow_mut();
        let owner = slot.get_or_insert_with(LlmCostBudget::default);
        let mut ledger = owner.0.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut ledger)
    })
}

pub(super) fn with_token_budget<T>(f: impl FnOnce(&mut TokenLedger) -> T) -> T {
    LLM_TOKEN_BUDGET.with(|slot| {
        let mut slot = slot.borrow_mut();
        let owner = slot.get_or_insert_with(LlmTokenBudget::default);
        let mut ledger = owner.0.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut ledger)
    })
}

// Establish uncapped accounting before cloning a parent. Fan-out must share
// totals even when no ceiling is installed. Empty swap temporaries stay None,
// so poll-enter and poll-exit only move pointers, without allocating ledgers.
pub(crate) fn capture_llm_cost_budget() -> Option<LlmCostBudget> {
    LLM_COST_BUDGET.with(|slot| {
        Some(
            slot.borrow_mut()
                .get_or_insert_with(LlmCostBudget::default)
                .clone(),
        )
    })
}

pub(crate) fn capture_llm_token_budget() -> Option<LlmTokenBudget> {
    LLM_TOKEN_BUDGET.with(|slot| {
        Some(
            slot.borrow_mut()
                .get_or_insert_with(LlmTokenBudget::default)
                .clone(),
        )
    })
}

/// Restores the previous cost owner when the installed owner is still active.
/// Cancellation may drop this guard outside its task's poll; in that case it
/// must leave the polling thread's unrelated owner alone.
#[must_use = "dropping the guard immediately restores the prior LLM cost budget"]
pub struct LlmBudgetGuard {
    previous: Option<LlmCostBudget>,
    installed: LlmCostBudget,
}

impl LlmBudgetGuard {
    /// Share this installation with a host that must re-arm it outside a poll.
    pub fn handle(&self) -> LlmCostBudgetHandle {
        LlmCostBudgetHandle(self.installed.clone())
    }
    /// Read this installation's accumulated spend, including inherited fan-out.
    /// Unlike an ambient peek, this remains tied to the owner when a suspended
    /// future is cancelled outside its poll or another task is active.
    pub fn total_cost(&self) -> f64 {
        self.handle().total_cost()
    }
}

impl Drop for LlmBudgetGuard {
    fn drop(&mut self) {
        LLM_COST_BUDGET.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|owner| Arc::ptr_eq(&owner.0, &self.installed.0))
            {
                *slot = self.previous.clone();
            }
        });
    }
}

/// Install fresh dispatch accounting with a dollar ceiling. Completed calls
/// exceeding the cap return a `BudgetExceeded` error, rendered as HTTP 429.
pub fn install_llm_cost_budget(max_cost_usd: f64) -> LlmBudgetGuard {
    install_llm_cost_budget_seeded(Some(max_cost_usd), 0.0)
}

/// Install a fresh cost owner seeded with durable session spend. Read
/// [`peek_total_cost`] before dropping the guard to persist the new total.
pub fn install_llm_cost_budget_seeded(max_cost_usd: Option<f64>, spent_usd: f64) -> LlmBudgetGuard {
    let installed = LlmCostBudget(Arc::new(Mutex::new(CostLedger {
        max: normalize_cost_ceiling(max_cost_usd),
        spent: if spent_usd.is_finite() {
            spent_usd.max(0.0)
        } else {
            0.0
        },
        observed: ObservedSessionUsage::default(),
    })));
    let previous = swap_llm_cost_budget(Some(installed.clone()));
    LlmBudgetGuard {
        previous,
        installed,
    }
}

/// Restores the previous token owner without changing an unrelated task on
/// cancellation. Cost and token guards can be installed independently.
#[must_use = "dropping the guard immediately restores the prior LLM token budget"]
pub struct LlmTokenBudgetGuard {
    previous: Option<LlmTokenBudget>,
    installed: LlmTokenBudget,
}

impl LlmTokenBudgetGuard {
    /// Share this installation with a host that must re-arm it outside a poll.
    pub fn handle(&self) -> LlmTokenBudgetHandle {
        LlmTokenBudgetHandle(self.installed.clone())
    }
}

impl Drop for LlmTokenBudgetGuard {
    fn drop(&mut self) {
        LLM_TOKEN_BUDGET.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|owner| Arc::ptr_eq(&owner.0, &self.installed.0))
            {
                *slot = self.previous.clone();
            }
        });
    }
}

/// Install fresh accounting with an input + output token ceiling.
pub fn install_llm_token_budget(max_tokens: u64) -> LlmTokenBudgetGuard {
    install_llm_token_budget_seeded(Some(max_tokens), 0)
}

/// Install fresh token accounting, optionally seeded and uncapped.
pub fn install_llm_token_budget_seeded(max_tokens: Option<u64>, spent: u64) -> LlmTokenBudgetGuard {
    let installed = LlmTokenBudget(Arc::new(Mutex::new(TokenLedger {
        max: max_tokens,
        spent,
    })));
    let previous = swap_llm_token_budget(Some(installed.clone()));
    LlmTokenBudgetGuard {
        previous,
        installed,
    }
}

/// Total dollar spend in the active cost owner's execution tree.
pub fn peek_total_cost() -> f64 {
    with_cost_budget(|budget| budget.spent)
}

/// Total tokens in the active token owner's execution tree.
pub fn peek_total_tokens() -> u64 {
    with_token_budget(|budget| budget.spent)
}

/// Re-arm the active execution tree's ceiling, preserving spend and observed
/// usage. `None` clears the cap; [`install_llm_cost_budget`] starts fresh.
pub fn set_llm_cost_budget(max_cost_usd: Option<f64>) {
    with_cost_budget(|budget| budget.max = normalize_cost_ceiling(max_cost_usd));
}

/// Re-arm the active execution tree's token ceiling without resetting spend.
pub fn set_llm_token_budget(max_tokens: Option<u64>) {
    with_token_budget(|budget| budget.max = max_tokens);
}

/// Active execution tree's cost ceiling, or `None` when uncapped.
pub fn peek_llm_cost_budget() -> Option<f64> {
    with_cost_budget(|budget| budget.max)
}

/// Active execution tree's token ceiling, or `None` when uncapped.
pub fn peek_llm_token_budget() -> Option<u64> {
    with_token_budget(|budget| budget.max)
}
