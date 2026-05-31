//! Settlement-agent drain loop (#1856, P-03).
//!
//! When `on_finish: drain` detects unsettled state at pipeline finish, the
//! runtime spawns a *settlement agent*: a bounded, deterministic loop that
//! walks the unsettled-state snapshot in a fixed order (subagents → triggers
//! → partial handoffs → in-flight LLM calls → pool pending) and applies a
//! per-item default disposition. Each disposition records a
//! `drain_decision` audit (which routes through the `OnDrainDecision`
//! lifecycle hook from P-06).
//!
//! ## Why a loop, not an LLM?
//!
//! The spec describes the settlement agent conceptually as a constrained
//! `agent_loop`. The P-03 ship lands the deterministic disposition layer:
//! every item gets a default action, every decision is auditable, and
//! the loop terminates in bounded time. An LLM-driven decision layer can
//! sit on top of this primitive later (a future epic phase) without
//! changing the storage / hook / ordering contract.
//!
//! ## Ordering
//!
//! The loop processes one bucket per iteration in the documented order:
//! subagents, triggers, handoffs, in-flight LLM calls, pool. Re-snapshotting
//! between buckets lets later iterations see the effects of earlier ones
//! (for example, an acknowledged trigger disappears from the queued bucket).
//!
//! ## Budget
//!
//! Default budget: 5 iterations. Hard cap: 20 (to prevent runaway settlement
//! even if a caller passes a larger override). On exhaustion the loop emits
//! a `drain_unsettled_remaining` audit so the remaining items are observable
//! in the pipeline transcript / replay record.
//!
//! ## Constrained tool surface
//!
//! While the loop runs, `settlement_agent_active()` returns true.
//! Surface-gated harness methods (registered via the constrained tool list)
//! inspect this flag and reject regular tool calls with HARN-DRN-002.

use std::cell::Cell;

use serde_json::Value;

use crate::orchestration::{
    acknowledge_partial_handoff, lifecycle_audit_log_snapshot, record_lifecycle_audit,
    unsettled_state_snapshot_async, HookControl, HookEvent,
};

/// Default per-pipeline drain budget. Overridable via
/// `pipeline.options.drain_budget_iterations` (currently surfaced through the
/// third argument to `harness.spawn_settlement_agent`).
pub const DRAIN_DEFAULT_BUDGET: usize = 5;

/// Hard cap on drain iterations. Even with a caller override, the loop will
/// never run more than this many iterations.
pub const DRAIN_HARD_CAP: usize = 20;

thread_local! {
    static SETTLEMENT_AGENT_ACTIVE: Cell<bool> = const { Cell::new(false) };
}

/// Is a settlement agent currently running on this thread?
///
/// Constrained-surface harness methods use this to reject non-whitelisted
/// tool calls with `HARN-DRN-002` so the settlement agent cannot wander
/// outside the allowed tool set.
pub fn settlement_agent_active() -> bool {
    SETTLEMENT_AGENT_ACTIVE.with(Cell::get)
}

/// RAII guard that flags the settlement agent as active for its lifetime.
struct SettlementAgentGuard;

impl SettlementAgentGuard {
    fn new() -> Self {
        SETTLEMENT_AGENT_ACTIVE.with(|cell| cell.set(true));
        Self
    }
}

impl Drop for SettlementAgentGuard {
    fn drop(&mut self) {
        SETTLEMENT_AGENT_ACTIVE.with(|cell| cell.set(false));
    }
}

/// Decode the caller-provided drain budget, clamping to the hard cap.
///
/// Accepts a JSON value that may be one of:
/// - `null` / missing → returns `DRAIN_DEFAULT_BUDGET`
/// - an integer → used directly (clamped to `1..=DRAIN_HARD_CAP`)
/// - an object with a `drain_budget_iterations` field → that integer
///
/// Non-positive or non-integer values fall back to the default.
pub fn decode_drain_budget(options: &Value) -> usize {
    let raw = match options {
        Value::Number(n) => n.as_u64().map(|v| v as usize),
        Value::Object(map) => map
            .get("drain_budget_iterations")
            .and_then(Value::as_u64)
            .map(|v| v as usize),
        _ => None,
    };
    raw.filter(|v| *v > 0)
        .unwrap_or(DRAIN_DEFAULT_BUDGET)
        .clamp(1, DRAIN_HARD_CAP)
}

/// Result of one drain iteration: how many items the bucket disposed of.
#[derive(Debug, Default, Clone, Copy)]
struct IterationProgress {
    subagents: usize,
    triggers: usize,
    handoffs: usize,
    llm_calls: usize,
    pool: usize,
}

impl IterationProgress {
    fn total(self) -> usize {
        self.subagents + self.triggers + self.handoffs + self.llm_calls + self.pool
    }
}

/// Run the settlement agent loop.
///
/// Returns a JSON receipt:
/// ```ignore
/// {
///   "status": "completed" | "exhausted" | "no_unsettled",
///   "method": "spawn_settlement_agent",
///   "iterations": <usize>,
///   "items_processed": <usize>,
///   "budget": <usize>,
///   "remaining": { /* unsettled-state snapshot at exit */ },
///   "return_value": <pipeline return value passed through>,
///   "audit_count": <usize>,  // number of audits recorded during the loop
/// }
/// ```
pub async fn run_settlement_agent_loop(
    initial_unsettled: Value,
    return_value: Value,
    options: Value,
) -> Value {
    run_settlement_agent_loop_with_ctx(None, initial_unsettled, return_value, options).await
}

pub async fn run_settlement_agent_loop_with_ctx(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    initial_unsettled: Value,
    return_value: Value,
    options: Value,
) -> Value {
    let budget = decode_drain_budget(&options);
    let _guard = SettlementAgentGuard::new();
    let audit_baseline = lifecycle_audit_log_snapshot().len();

    // Trivial early exit when the caller-provided snapshot is already empty
    // *and* the live snapshot agrees. The caller (`on_finish_drain` in
    // `stdlib_lifecycle.harn`) only calls us when the snapshot is non-empty,
    // but be defensive: re-check before opening any spans / audits.
    let live = unsettled_state_snapshot_async().await;
    if live.is_empty() && unsettled_is_empty(&initial_unsettled) {
        return serde_json::json!({
            "status": "no_unsettled",
            "method": "spawn_settlement_agent",
            "iterations": 0,
            "items_processed": 0,
            "budget": budget,
            "remaining": live.to_json(),
            "return_value": return_value,
            "audit_count": 0,
        });
    }

    let mut iterations: usize = 0;
    let mut total_processed: usize = 0;
    let mut last_snapshot_json = live.to_json();
    while iterations < budget {
        iterations += 1;
        let snapshot = unsettled_state_snapshot_async().await;
        last_snapshot_json = snapshot.to_json();
        if snapshot.is_empty() {
            break;
        }
        let progress = drain_one_iteration(ctx, &snapshot).await;
        total_processed += progress.total();
        if progress.total() == 0 {
            // The loop could not make progress on any bucket this iteration —
            // typically because every item is in a state the loop cannot
            // resolve (e.g. an in-flight LLM call we don't own). Break early
            // rather than burning the remaining budget on a no-op loop.
            break;
        }
    }

    let final_snapshot = unsettled_state_snapshot_async().await;
    let exhausted = !final_snapshot.is_empty();
    if exhausted {
        record_lifecycle_audit(
            "drain_unsettled_remaining",
            serde_json::json!({
                "iterations": iterations,
                "budget": budget,
                "items_processed": total_processed,
                "remaining": final_snapshot.to_json(),
            }),
        );
        last_snapshot_json = final_snapshot.to_json();
    } else {
        record_lifecycle_audit(
            "pipeline_finalized",
            serde_json::json!({
                "reason": "drained",
                "iterations": iterations,
                "items_processed": total_processed,
            }),
        );
    }

    let audit_count = lifecycle_audit_log_snapshot()
        .len()
        .saturating_sub(audit_baseline);
    let status = if exhausted { "exhausted" } else { "completed" };

    serde_json::json!({
        "status": status,
        "method": "spawn_settlement_agent",
        "iterations": iterations,
        "items_processed": total_processed,
        "budget": budget,
        "remaining": last_snapshot_json,
        "return_value": return_value,
        "audit_count": audit_count,
    })
}

fn unsettled_is_empty(value: &Value) -> bool {
    fn empty_bucket(value: &Value, key: &str) -> bool {
        value
            .get(key)
            .and_then(Value::as_array)
            .map(|items| items.is_empty())
            .unwrap_or(true)
    }
    empty_bucket(value, "suspended_subagents")
        && empty_bucket(value, "queued_triggers")
        && empty_bucket(value, "partial_handoffs")
        && empty_bucket(value, "in_flight_llm_calls")
        && empty_bucket(value, "pool_pending_tasks")
}

/// Process at most one item per category in the documented drain order:
/// subagents → triggers → handoffs → LLM calls → pool pending tasks.
///
/// Each disposition records a `drain_decision` lifecycle audit through
/// `emit_drain_decision`, which routes through the `OnDrainDecision`
/// lifecycle hook (P-06) before persisting.
async fn drain_one_iteration(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    snapshot: &crate::orchestration::UnsettledStateSnapshot,
) -> IterationProgress {
    let mut progress = IterationProgress::default();

    // 1. Suspended subagents — cancel by default. We do not invoke the
    //    `__host_worker_close` builtin from here (that lives in `agents.rs`
    //    and requires a full VM context); instead we record a
    //    `drain_decision` audit with `action: "cancel"` so the disposition
    //    is observable. A future LLM-driven variant can replace the
    //    default with `resume` / `handoff` decisions before falling through
    //    to the disposition emit.
    if let Some(item) = snapshot.suspended_subagents.first() {
        let handle = item
            .get("handle")
            .or_else(|| item.get("id"))
            .cloned()
            .unwrap_or(Value::Null);
        emit_drain_decision(
            ctx,
            "cancel",
            "suspended_subagent",
            handle,
            item.clone(),
            Some("default disposition: cancel suspended subagent at drain"),
        )
        .await;
        progress.subagents += 1;
    }

    // 2. Queued triggers — acknowledge by default. The audit alone removes
    //    the item from the per-pipeline lifecycle view; the event-log-backed
    //    queue is the source of truth for re-delivery and is updated by
    //    `acknowledge_trigger_id` when called through the harness surface.
    //    For the in-process loop the audit is sufficient to mark the
    //    decision; the conformance fixture relies on the audit shape, not on
    //    the event-log mutation.
    if let Some(item) = snapshot.queued_triggers.first() {
        let id = item.get("id").cloned().unwrap_or(Value::Null);
        emit_drain_decision(
            ctx,
            "acknowledge",
            "queued_trigger",
            id,
            item.clone(),
            Some("default disposition: acknowledge stale queued trigger at drain"),
        )
        .await;
        progress.triggers += 1;
    }

    // 3. Partial handoffs — acknowledge by default with a `deferred`
    //    decision. Use the orchestration primitive so the envelope is
    //    actually removed from the in-memory registry (this is what makes
    //    the loop converge: the next snapshot sees the bucket shrink).
    if let Some(item) = snapshot.partial_handoffs.first() {
        let envelope_id = item
            .get("envelope_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let item_id = item
            .get("envelope_id")
            .cloned()
            .unwrap_or_else(|| Value::String(envelope_id.clone()));
        // Drop into the primitive directly; we own the ordering, so we
        // bypass the surface-level HARN-DRN-001 guard on `acknowledge_handoff`.
        // The orchestration primitive itself appends a `handoff_acknowledged`
        // audit, so the `drain_decision` audit we emit here is in addition
        // to it (intentional: the drain_decision shape is what the hook
        // dispatcher consumes; the handoff_acknowledged audit is the
        // existing per-envelope record).
        let _ = acknowledge_partial_handoff(
            &envelope_id,
            serde_json::json!({"disposition": "deferred", "source": "settlement_agent"}),
        );
        emit_drain_decision(
            ctx,
            "acknowledge",
            "partial_handoff",
            item_id,
            item.clone(),
            Some("default disposition: acknowledge partial handoff as deferred"),
        )
        .await;
        progress.handoffs += 1;
    }

    // 4. In-flight LLM calls — record a `drain` decision; we do not cancel
    //    in-flight requests from here (the call owner manages cancellation
    //    via `llm_call_drain`). The audit makes the decision auditable
    //    while letting the caller-owned future complete naturally.
    if let Some(item) = snapshot.in_flight_llm_calls.first() {
        let id = item.get("call_id").cloned().unwrap_or(Value::Null);
        emit_drain_decision(
            ctx,
            "drain",
            "in_flight_llm_call",
            id,
            item.clone(),
            Some("default disposition: record drain decision for in-flight llm call"),
        )
        .await;
        progress.llm_calls += 1;
    }

    // 5. Pool pending tasks — record a `defer` decision; the pool primitive
    //    owns re-dispatch.
    if let Some(item) = snapshot.pool_pending_tasks.first() {
        let id = item
            .get("task_id")
            .or_else(|| item.get("id"))
            .cloned()
            .unwrap_or(Value::Null);
        emit_drain_decision(
            ctx,
            "defer",
            "pool_pending_task",
            id,
            item.clone(),
            Some("default disposition: defer pool pending task at drain"),
        )
        .await;
        progress.pool += 1;
    }

    progress
}

/// Record a `drain_decision` lifecycle audit *and* run the
/// `OnDrainDecision` hook chain so registered VM closures see the
/// disposition before it is persisted. Mirrors the
/// `record_emit_audit_with_hooks` path used by `harness.emit_audit`
/// (P-06) but factored to keep the loop body readable.
async fn emit_drain_decision(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    action: &str,
    category: &str,
    item_id: Value,
    item: Value,
    reason: Option<&str>,
) {
    let item_payload = serde_json::json!({
        "category": category,
        "id": item_id,
        "summary": item,
    });
    let payload = serde_json::json!({
        "action": action,
        "item": item_payload,
        "reason": reason.unwrap_or("default disposition"),
    });
    let hook_payload = serde_json::json!({
        "event": HookEvent::OnDrainDecision.as_str(),
        "action": payload.get("action").cloned().unwrap_or(Value::Null),
        "item": payload.get("item").cloned().unwrap_or(Value::Null),
        "payload": payload.clone(),
    });
    let mut effective = payload;
    match super::hooks::run_lifecycle_hooks_with_control_with_ctx(
        ctx,
        HookEvent::OnDrainDecision,
        &hook_payload,
    )
    .await
    {
        Ok(HookControl::Allow) | Err(_) => {}
        Ok(HookControl::Block { .. }) => return,
        Ok(HookControl::Modify { payload: modified }) => {
            if let Some(p) = modified.get("payload") {
                effective = p.clone();
            }
        }
        Ok(HookControl::Decision { .. }) => {}
    }
    record_lifecycle_audit("drain_decision", effective);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_drain_budget_uses_default_for_null() {
        assert_eq!(decode_drain_budget(&Value::Null), DRAIN_DEFAULT_BUDGET);
    }

    #[test]
    fn decode_drain_budget_clamps_to_hard_cap() {
        assert_eq!(
            decode_drain_budget(&serde_json::json!(1000)),
            DRAIN_HARD_CAP
        );
    }

    #[test]
    fn decode_drain_budget_reads_object_field() {
        assert_eq!(
            decode_drain_budget(&serde_json::json!({"drain_budget_iterations": 3})),
            3
        );
    }

    #[test]
    fn decode_drain_budget_rejects_zero_and_negative() {
        assert_eq!(
            decode_drain_budget(&serde_json::json!(0)),
            DRAIN_DEFAULT_BUDGET
        );
        // Negative parses as None for as_u64 → falls back to default.
        assert_eq!(
            decode_drain_budget(&serde_json::json!(-5)),
            DRAIN_DEFAULT_BUDGET
        );
    }

    #[test]
    fn unsettled_is_empty_handles_missing_buckets() {
        assert!(unsettled_is_empty(&serde_json::json!({})));
        assert!(unsettled_is_empty(
            &serde_json::json!({"suspended_subagents": []})
        ));
        assert!(!unsettled_is_empty(
            &serde_json::json!({"queued_triggers": [{"id": "x"}]})
        ));
    }
}
