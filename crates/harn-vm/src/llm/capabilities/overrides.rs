//! Scoped project capability override context.

use std::cell::RefCell;

use super::model::CapabilitiesFile;

thread_local! {
    /// Capability overrides for the currently-polled Harn execution.
    ///
    /// The ambient execution scope swaps this context per poll for embedded
    /// hosts, so a capability decision never crosses from one ACP server into
    /// another while futures interleave.
    static LLM_CAPABILITY_OVERRIDES_CONTEXT: RefCell<Option<CapabilitiesFile>> = const { RefCell::new(None) };
}

pub(super) fn current_user_overrides() -> Option<CapabilitiesFile> {
    LLM_CAPABILITY_OVERRIDES_CONTEXT.with(|cell| cell.borrow().clone())
}

pub(super) fn set_user_overrides(file: Option<CapabilitiesFile>) {
    LLM_CAPABILITY_OVERRIDES_CONTEXT.with(|cell| *cell.borrow_mut() = file);
}

/// Swap the per-execution capability overrides and return the previous value.
///
/// Hosts configure this through `orchestration::scope_llm_runtime_overrides`
/// rather than retaining a guard across an asynchronous turn.
pub(crate) fn swap_user_overrides(next: Option<CapabilitiesFile>) -> Option<CapabilitiesFile> {
    LLM_CAPABILITY_OVERRIDES_CONTEXT.with(|cell| std::mem::replace(&mut *cell.borrow_mut(), next))
}
