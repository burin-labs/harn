# Pipeline lifecycle cookbook

Five end-to-end patterns for the callback-first pipeline lifecycle
(epic #1853). Each recipe is a complete, copy-paste starting point.
For the surface reference see [Pipeline lifecycle](../pipeline-lifecycle.md);
for the per-preset stdlib reference see
[Pipeline lifecycle presets](../stdlib/lifecycle.md); for the
LLM-friendly quickref see the "Pipeline lifecycle" section in
`docs/llm/harn-quickref.md`.

The recipes use `harn,ignore` fences because they wire up full
multi-pipeline topologies (producer pipeline, drain pipeline, trigger
handlers, host integration glue). Each fragment type-checks; the
orchestration loop assumes a host that runs the constituent pipelines
(such as `harn orchestrator`).

## 1. Nightly settlement handoff

A live ingest pipeline triages events synchronously up to a watermark,
then hands deferred work off to a separate nightly settlement
pipeline. The handoff envelope carries the unsettled snapshot; the
nightly pipeline drains it under its own schedule and budget.

```harn,ignore
import { trigger_register } from "std/triggers"
import { on_finish_handoff_to } from "std/lifecycle"

// --- live ingest pipeline ---
pipeline ingest(events) {
  // Any deferred items become partial-handoff envelopes via
  // harness.handoff_to so they show up in unsettled_state().
  pipeline_on_finish(on_finish_handoff_to("nightly-settle", {
    note: "live ingest deferred bucket",
  }))

  for event in events {
    let outcome = try_process_now(event)
    if outcome.deferred {
      // Stage the item directly on the harness so the on_finish
      // callback picks it up from harness.unsettled_state().
      __handoff_to_settle(event)
    }
  }
  return {status: "ingest_complete", count: len(events)}
}

// --- nightly settlement pipeline ---
pipeline nightly_settle(envelope) {
  let items = envelope.unsettled.partial_handoffs
  for item in items {
    settle_one(item.payload)
  }
  return {status: "settled", count: len(items)}
}

fn __handoff_to_settle(event) {
  // The harness builtin is provided by the runtime inside any
  // pipeline body; declared as a helper so the snippet reads
  // top-to-bottom.
  current_harness().handoff_to("nightly-settle", event)
}
```

Why `on_finish_handoff_to` over emitting a channel: the handoff
envelope carries the full unsettled snapshot, with origin and timing
metadata, and the nightly pipeline gets the bucket in one call instead
of replaying N channel emits. The handoff is recorded as a
`partial_handoff_envelope` audit, so the replay oracle can verify the
same envelope crosses both pipelines on a second run.

Why a separate pipeline: the nightly run owns its own budget, hook
chain, and on_finish policy. A failure mid-settle does not unwind the
live ingest run, and a `pre_finish` block on the nightly pipeline
holds finish open until the per-item drain completes.

## 2. Audit every drain decision to a custom store

A regulated team needs every disposition the settlement-agent loop
makes to land in an external audit store (Splunk, BigQuery, S3), not
just the per-run `pipeline.lifecycle.audit` topic. A
`register_session_hook("on_drain_decision", ...)` callback observes
each decision before it persists, and a composed `on_finish` callback
runs the drain loop with telemetry around it.

```harn,ignore
import { on_finish_drain } from "std/lifecycle"
import { compose, with_telemetry } from "std/lifecycle/combinators"

pipeline triage(input) {
  // 1. Mirror every drain disposition to our external store.
  register_session_hook("on_drain_decision", { event ->
    audit_store_push({
      pipeline: event.pipeline_id,
      bucket: event.bucket,
      item_id: event.item_id,
      disposition: event.disposition,
      seq: event.seq,
      at_ms: event.at_ms,
    })
    // Return nil to allow; we are not vetoing or rewriting.
    return nil
  })

  // 2. Drain on finish, wrapped in an OTel span so dashboards can
  //    pin on the duration distribution.
  pipeline_on_finish(
    with_telemetry(on_finish_drain, "triage_drain"),
  )

  return process(input)
}

fn audit_store_push(record) {
  // Replace with the real client.
  __external_audit_emit(record)
}
```

Why a hook over inline logging in a custom on_finish: the
`on_drain_decision` gate fires once per item the settlement-agent loop
processes, regardless of which preset or custom callback drives the
loop. A hook captures every disposition uniformly — including from
future presets — without forking the drain logic.

Why `with_telemetry` outside the drain: the wrapper emits
`triage_drain_started` / `_completed` / `_errored` audit entries with
a stable span name, so the external store sees a paired pair of
records bracketing the per-item entries.

## 3. Long-paused agent with resume-continuity reminder

A research agent self-parks via `agent_await_resumption` and may
sleep for hours. When it resumes, two things must happen exactly
once: a continuity reminder lands on the first resumed turn (the
runtime injects this by default), and a custom telemetry envelope
fires so the operator dashboard shows the pause duration.

```harn,ignore
import { spawn_agent, parse_resume_conditions } from "std/agent/workers"

pipeline research_runner(task) {
  // Bracket the suspend / resume gates with telemetry. The runtime
  // also fires its built-in `resume_continuity` reminder; this hook
  // wraps it with structured operator telemetry.
  register_session_hook("post_resume", { event ->
    operator_dashboard_emit("agent.resumed", {
      handle: event.worker.handle,
      suspended_at_ms: event.suspended_at_ms,
      resumed_at_ms: event.resumed_at_ms,
      duration_ms: event.resumed_at_ms - event.suspended_at_ms,
      reason: event.suspend_reason,
      resume_cause: event.resume_cause,
    })
    return nil
  })

  let resume_when = parse_resume_conditions({
    timeout: {duration_minutes: 240, on_timeout: "resume_with_summary"},
    on_event: "operator.resume",
  })

  let worker = spawn_agent({
    prompt: task,
    system: "Pause when you need a long-running external lookup.",
    options: {resume_when: resume_when},
  })

  return wait_agent(worker)
}
```

Why a `post_resume` hook over polling the worker registry: the hook
runs once per resume, on the right session, with the suspend and
resume timestamps already paired. Polling drifts and can double-fire
across worker restarts.

Why parse `resume_when` upfront: the parsed dict is captured on the
suspend snapshot, so a cold-restore (`harn run --resume <path>`)
reconstitutes the same wake conditions instead of re-evaluating the
shape at resume time.

## 4. Hook-based supervision — deny suspend during business hours

A SaaS team wants to prevent any agent from self-parking between 9 AM
and 5 PM local time (so the on-call rotation can intervene with full
context). A `pre_suspend` hook denies the suspend; the runtime keeps
the worker running and the agent gets a reminder explaining the policy.

```harn,ignore
fn during_business_hours(now_ms) -> bool {
  let hour = wall_clock_hour_local(now_ms)
  return hour >= 9 && hour < 17
}

pipeline supervised_agents() {
  register_session_hook("pre_suspend", { event ->
    if !during_business_hours(now_ms()) {
      return nil   // allow
    }
    return {
      block: true,
      reason: "agents may not self-park during business hours; ask on-call",
      reminder: {
        body: "Your suspend was denied (business-hours policy). Continue working or escalate via the on-call channel.",
        tags: ["policy_violation", "business_hours_no_suspend"],
        ttl_turns: 1,
        dedupe_key: "policy.no_suspend_business_hours",
      },
    }
  })

  // ... rest of the setup pipeline; agents spawn under this hook
  // chain inherit the deny policy.
  return nil
}
```

Why `pre_suspend` over `OnPersonaPaused`: `pre_suspend` is the
*gate*; it can veto the suspend and keep the worker running. The
`Paused` events fire after the suspend is already committed.

Why a paired reminder: the agent that tried to self-park needs to see
*why* it was denied, not just continue blindly. The 1-turn TTL
ensures the reminder lands on the very next turn and then evaporates
so it does not pollute later context.

Why the dedupe key: if the agent retries the suspend three times in
one turn, the dedupe collapses the reminder to one entry rather than
stacking three identical bodies.

## 5. Replay-deterministic test harness for a multi-suspend pipeline

A pipeline suspends a worker, waits for an external event, resumes,
suspends again, and finally drains. The conformance fixture needs to
replay deterministically — same audit entries, same disposition order,
same handoff envelopes — across two runs of the same pipeline.

```harn,ignore
import { mock_time, advance_time } from "std/clock"
import { flush_trigger_aggregations } from "std/triggers/testing"
import { pipeline_lifecycle_audit_log_take } from "std/lifecycle"
import { on_finish_drain } from "std/lifecycle"
import { compose, with_telemetry } from "std/lifecycle/combinators"

pipeline multi_suspend_fixture() {
  // 1. Pin wall-clock so queued_at_ms / age_ms are reproducible.
  mock_time(1_700_000_000_000)

  // 2. Capture audits via the per-run log instead of wall-clock spans.
  pipeline_on_finish(
    with_telemetry(on_finish_drain, "fixture_drain"),
  )

  // 3. Run the workload. Each suspend / resume / drain step records
  //    a typed audit entry.
  let worker = spawn_research_worker()
  emit_external_event("operator.resume", {})
  advance_time(60_000)
  flush_trigger_aggregations()

  let worker2 = spawn_followup_worker(worker)
  emit_external_event("operator.resume", {})
  advance_time(120_000)
  flush_trigger_aggregations()

  return wait_agent(worker2)
}

pipeline assert_replay_determinism() {
  // 4. Drain the audit log; the conformance harness compares this
  //    byte-for-byte against the recorded fixture.
  let entries = pipeline_lifecycle_audit_log_take()
  return {audits: entries, count: len(entries)}
}
```

Why `mock_time` + `advance_time` over wall-clock: the harness banlist
(`make lint-test-patterns`) prohibits `std::thread::sleep`,
`Instant::now()` polling, and short `recv_timeout` calls in tests.
Mocking the clock lets `queued_at_ms` and `age_ms` fields land at
deterministic offsets that the replay oracle can compare directly.

Why `flush_trigger_aggregations` before the second suspend: batched
trigger handlers buffer events until the aggregation window closes.
Explicit flushes turn an inherently time-driven boundary into a
synchronous one so each step's audit entries land before the next
step opens.

Why `pipeline_lifecycle_audit_log_take` over snapshot: the take
variant drains the log so a subsequent assertion starts from a clean
slate. The conformance fixture asserts on the drained list as a
whole, not on a snapshot that might include leftovers from a previous
test.

## Picking the right primitive

For any of these recipes, the wrong tool is also worth knowing:

- **Don't use a `post_finish` hook to block.** `post_finish` is
  advisory; the value is already captured. Use `pre_finish` (which
  rejects `block` and points you at the right primitive) or
  `on_finish_block_until_settled` to delay finish until work drains.
- **Don't write a custom drain loop unless you have to.**
  `on_finish_drain` already walks the buckets in the canonical
  order, respects `HARN-DRN-001` ordering enforcement, and fires
  `on_drain_decision` per item. A custom loop has to re-derive every
  one of those properties.
- **Don't reach for `pre_suspend` denials to throttle suspends.**
  The runtime emits an audit per attempt; a deny chain quickly
  becomes noise. Use a worker-side budget (`OnBudget.terminate` or
  `graceful_exit`) instead.
- **Don't poll `harness.unsettled_state()` from a tight loop.** Each
  call walks the live registries plus the event log. Use
  `harness.wait_for_any_settlement(max_duration)` or
  `on_finish_block_until_settled` to wait on a single coalesced
  snapshot.
