//! Call-scoped measurement of physical provider dispatches.
//!
//! How many provider requests a terminal represents has exactly one authority:
//! the observed transport boundary, which records one dispatch here per
//! request. Every other surface reads this ledger rather than inferring a
//! count from tokens, attempts, or error text.
//!
//! Before this module the counter lived only on the agent-session host, so
//! `record_provider_dispatch` was a silent no-op whenever no agent session was
//! open. `agent_loop` reported a correct count while a bare `llm_call`
//! measured nothing, and the structured envelope then fell back to asserting
//! one unknown attempt for a terminal that never dispatched. A budget refusal
//! that made no request was reported as one unpriced call, which is what
//! charged a downstream host's cost meter a conservative reserve per refusal
//! (burin-labs/harn#8529).
//!
//! The ledger is a `tokio::task_local`, matching `raw_provider_capture` and
//! `cost_route` in this same layer: a scope follows its future across `.await`
//! and across spawned work instead of pinning to whichever thread happened to
//! poll it.

use std::future::Future;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

tokio::task_local! {
    static PROVIDER_DISPATCH_LEDGER: ProviderDispatchLedger;
}

/// Physical provider dispatches observed inside one open measurement scope.
///
/// Cloning shares the same underlying count, so the scope's owner and the
/// deep call sites that record into it see one number. A scope opened inside
/// another keeps a link to it, and a dispatch is recorded on every scope up
/// that chain: the inner call reports its own requests while the enclosing
/// call still sees the total.
#[derive(Clone, Debug, Default)]
pub(crate) struct ProviderDispatchLedger {
    dispatches: Arc<AtomicI64>,
    parent: Option<Box<ProviderDispatchLedger>>,
}

impl ProviderDispatchLedger {
    /// Dispatches recorded so far. Zero here is a measured zero: the scope was
    /// open and nothing dispatched.
    pub(crate) fn observed(&self) -> i64 {
        self.dispatches.load(Ordering::Relaxed)
    }

    fn record(&self) {
        self.dispatches.fetch_add(1, Ordering::Relaxed);
        if let Some(parent) = self.parent.as_deref() {
            parent.record();
        }
    }

    /// The same counter, linked under whatever scope is currently open.
    fn nested_under_current(&self) -> Self {
        Self {
            dispatches: Arc::clone(&self.dispatches),
            parent: PROVIDER_DISPATCH_LEDGER
                .try_with(Clone::clone)
                .ok()
                .map(Box::new),
        }
    }
}

/// Run `future` with `ledger` as the open measurement scope.
///
/// Dispatches inside `future` land on `ledger` and on every enclosing scope,
/// so the caller reads exactly its own requests through its clone while an
/// outer measurement still totals them.
///
/// This is a plain `fn` that boxes `future` rather than an `async fn`. An
/// `async fn` wrapper embeds `F` inline in its own future, so every caller
/// awaiting it would carry the whole wrapped call future a second time on its
/// stack frame; the LLM call futures here are large enough that the doubling
/// trips the stack-frame budget. Boxing moves `F` to the heap, so the caller's
/// frame holds only a pointer. One heap allocation per provider call is
/// negligible against a network round trip.
pub(crate) fn with_provider_dispatch_ledger<F>(
    ledger: ProviderDispatchLedger,
    future: F,
) -> impl Future<Output = F::Output>
where
    F: Future,
{
    let scoped = ledger.nested_under_current();
    PROVIDER_DISPATCH_LEDGER.scope(scoped, Box::pin(future))
}

/// Stamp the scope's measurement onto a thrown terminal.
///
/// `agent_loop` already exposes `provider_call_count` on its thrown terminal;
/// a bare `llm_call` throw carried nothing, so a consumer catching it could
/// not tell a refusal before dispatch from a failure after one. Only a dict
/// terminal can carry the field, and a field the producer already set wins.
pub(crate) fn stamp_thrown_terminal(
    error: crate::value::VmError,
    ledger: &ProviderDispatchLedger,
) -> crate::value::VmError {
    use crate::value::{VmError, VmValue};
    match error {
        VmError::Thrown(VmValue::Dict(dict)) => {
            let mut dict = (*dict).clone();
            dict.entry(crate::value::intern_key("provider_call_count"))
                .or_insert_with(|| VmValue::Int(ledger.observed()));
            VmError::Thrown(VmValue::dict(dict))
        }
        other => other,
    }
}

/// Dispatches measured by the open scope, or `None` when no scope is open.
///
/// `Some(0)` and `None` are deliberately different answers. The first says a
/// measurement ran and saw no request; the second says nobody measured. A
/// consumer that collapses them reintroduces the defect this module exists to
/// remove.
pub(crate) fn measured_dispatches() -> Option<i64> {
    PROVIDER_DISPATCH_LEDGER
        .try_with(ProviderDispatchLedger::observed)
        .ok()
}

/// Record one physical provider dispatch against the open scope.
///
/// A dispatch with no scope open is deliberately not counted anywhere: the
/// absence of a scope is the absence of a measurement, and callers that need
/// one open it.
pub(crate) fn record_dispatch() {
    let _ = PROVIDER_DISPATCH_LEDGER.try_with(ProviderDispatchLedger::record);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_open_scope_with_no_dispatch_measures_zero() {
        let ledger = ProviderDispatchLedger::default();
        with_provider_dispatch_ledger(ledger.clone(), async {}).await;
        assert_eq!(ledger.observed(), 0);
    }

    #[tokio::test]
    async fn dispatches_reach_the_open_scope() {
        let ledger = ProviderDispatchLedger::default();
        with_provider_dispatch_ledger(ledger.clone(), async {
            record_dispatch();
            record_dispatch();
        })
        .await;
        assert_eq!(ledger.observed(), 2);
    }

    #[tokio::test]
    async fn a_nested_scope_reports_its_own_share_and_the_outer_the_total() {
        let outer = ProviderDispatchLedger::default();
        with_provider_dispatch_ledger(outer.clone(), async {
            record_dispatch();
            let inner = ProviderDispatchLedger::default();
            with_provider_dispatch_ledger(inner.clone(), async {
                record_dispatch();
                record_dispatch();
                assert_eq!(measured_dispatches(), Some(2));
            })
            .await;
            assert_eq!(inner.observed(), 2);
            assert_eq!(measured_dispatches(), Some(3));
        })
        .await;
        assert_eq!(outer.observed(), 3);
    }

    #[tokio::test]
    async fn a_thrown_dict_terminal_is_stamped_and_a_producer_stamp_wins() {
        use crate::value::{VmDictExt as _, VmError, VmValue};
        let ledger = ProviderDispatchLedger::default();
        with_provider_dispatch_ledger(ledger.clone(), async {
            record_dispatch();
        })
        .await;

        let mut bare = crate::value::DictMap::new();
        bare.put_str("category", "budget_exceeded");
        let stamped = stamp_thrown_terminal(VmError::Thrown(VmValue::dict(bare)), &ledger);
        let VmError::Thrown(VmValue::Dict(dict)) = stamped else {
            panic!("dict terminal survives stamping");
        };
        assert!(matches!(
            dict.get("provider_call_count"),
            Some(VmValue::Int(1))
        ));

        let mut owned = crate::value::DictMap::new();
        owned.insert(
            crate::value::intern_key("provider_call_count"),
            VmValue::Int(7),
        );
        let kept = stamp_thrown_terminal(VmError::Thrown(VmValue::dict(owned)), &ledger);
        let VmError::Thrown(VmValue::Dict(dict)) = kept else {
            panic!("dict terminal survives stamping");
        };
        assert!(matches!(
            dict.get("provider_call_count"),
            Some(VmValue::Int(7))
        ));

        let text = stamp_thrown_terminal(
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from("plain"))),
            &ledger,
        );
        assert!(matches!(text, VmError::Thrown(VmValue::String(_))));
    }

    #[tokio::test]
    async fn no_open_scope_reads_as_no_measurement_not_as_zero() {
        assert_eq!(measured_dispatches(), None);
        let ledger = ProviderDispatchLedger::default();
        with_provider_dispatch_ledger(ledger, async {
            assert_eq!(measured_dispatches(), Some(0));
        })
        .await;
    }

    #[tokio::test]
    async fn a_dispatch_outside_every_scope_is_not_attributed_to_a_later_one() {
        record_dispatch();
        let ledger = ProviderDispatchLedger::default();
        with_provider_dispatch_ledger(ledger.clone(), async {}).await;
        assert_eq!(ledger.observed(), 0);
    }
}
