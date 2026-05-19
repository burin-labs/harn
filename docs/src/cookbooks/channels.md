# Channel cookbook

Five end-to-end patterns for agent channels (#1870). Each recipe is a
complete, copy-paste starting point. For the surface reference see
[Agent channels](../agent-channels.md); for the LLM-friendly quickref
see the "Durable agent channels" section in `docs/llm/harn-quickref.md`.

The recipes use `harn,ignore` fences because they show full
multi-agent topologies (publisher pipeline + subscriber pipeline). Each
fragment type-checks; the orchestration loop assumes a host that runs
both pipelines (such as `harn orchestrator`).

## 1. Release handshake

One agent waits for another's emit before proceeding. The PR agent
emits `pr.merged` on each merge; a release agent batches three of them
into a single release run; downstream merge-captain agents in
burin-code and harn-cloud subscribe to the release's
`harn-release.shipped` event so they can rebase their queues.

```harn,ignore
import { trigger_register } from "std/triggers"

// --- harn-pr-agent ---
pipeline pr_agent_on_merge(pr) {
  emit_channel("pr.merged", {
    repo: "burin-labs/harn",
    number: pr.number,
    sha: pr.merge_commit_sha,
    target_branch: pr.target_branch,
  })
}

// --- release agent ---
pipeline release_agent_setup() {
  trigger_register({
    id: "release-after-3-merges",
    kind: "channel.emit",
    provider: "channel",
    match: {events: ["channel:pr.merged"]},
    when: { event -> event.provider_payload.payload.target_branch == "main" },
    batch: {count: 3, window: "2h", key: "repo", expire_action: "fire_partial"},
    handler: { event ->
      let merged = event.batch
      let repo = merged[0].provider_payload.payload.repo
      let shas = merged
        |> map({ e -> e.provider_payload.payload.sha })
      let release = cut_release(repo, shas)
      emit_channel("harn-release.shipped", {
        repo: repo,
        version: release.version,
        merged_prs: len(merged),
      })
    },
  })
}

// --- merge captains in burin-code / harn-cloud ---
pipeline merge_captain_setup() {
  trigger_register({
    id: "rebase-on-harn-release",
    kind: "channel.emit",
    provider: "channel",
    match: {events: ["channel:harn-release.shipped"]},
    handler: { event ->
      let release = event.provider_payload.payload
      rebase_queued_prs_against(release.version)
    },
  })
}
```

Why channels over a direct handoff: there is no single named recipient.
The release agent doesn't know about merge captains, and each
downstream service registers its own subscriber. The journal is the
contract.

Why `batch` over a counter in the handler: the count is process state;
the buffer is durable. A restart between PRs 2 and 3 doesn't lose the
batch.

Why a signed receipt: the replay oracle verifies the same three SHAs
fire the same release across two runs of the same pipeline. See
[CH-07 replay receipts](../agent-channels.md#observability).

## 2. Periodic check-in via tool-call counting

Inject a reflection prompt into a running agent loop every 30 tool
calls. The agent emits `tool_call.completed` on each tool use (via a
tool hook); a batched trigger drops a reminder onto the same session
when the counter hits 30.

```harn,ignore
import { trigger_register, ReminderInject } from "std/triggers"

pipeline reflection_agent(task) {
  // 1. Emit a channel event from a tool hook so we don't have to
  //    instrument every tool definition by hand.
  register_tool_hook({
    pattern: "*",
    post: { ctx ->
      emit_channel("tool_call.completed", {
        tool: ctx.tool_name,
        session: ctx.session_id,
      })
      return nil
    },
  })

  // 2. Subscribe to the channel with a batched ReminderInject. The
  //    reminder lands on the *same* session at the next turn boundary.
  trigger_register({
    id: "reflect-every-30-tools",
    kind: "channel.emit",
    provider: "channel",
    match: {events: ["channel:tool_call.completed"]},
    batch: {count: 30, window: "1h", key: "session"},
    handler: ReminderInject({
      target: "current",
      body: "You have used 30 tools without a checkpoint. Take this turn to summarize progress, re-read the spec, and adjust the plan if needed.",
      tags: ["reflection_nudge"],
      ttl_turns: 1,
      dedupe_key: "reflection_nudge",
    }),
  })

  // 3. Run the loop. The reflection nudge arrives transparently.
  agent_loop(task, "You are a careful engineering agent. Reflect when nudged.")
}
```

Why channels + `batch` + `ReminderInject` over a `post_turn_callback`
counter: the callback runs after every turn; this fires after every 30
tool *uses* regardless of how they distribute across turns. Two tools
in turn N and 28 across turn N+1 trips at the same point. The batch
counter resets cleanly after fire, and the durable buffer survives a
worker restart.

Why `dedupe_key`: if the agent stalls mid-reflection and 30 more tools
are dispatched, the next reminder replaces (rather than stacks on) the
pending one.

## 3. Multi-agent feedback loop

A planner agent drafts a plan; reviewer agents critique it; the
planner subscribes to the critiques and revises. Channels make this a
declarative cycle rather than a hand-rolled coordination dance.

```harn,ignore
import { trigger_register, ReminderInject } from "std/triggers"

// --- planner ---
pipeline planner_loop(task) {
  let session = agent_session_open("planner")
  let draft = llm_call(task, "Write a one-page plan.", {session_id: session})
  emit_channel("plan.draft", {
    plan: draft,
    revision: 1,
    session_id: session,
  })

  // Watch for reviewer feedback addressed at our session.
  trigger_register({
    id: "planner-on-feedback-" + session,
    kind: "channel.emit",
    provider: "channel",
    match: {events: ["channel:plan.feedback"]},
    when: { event -> event.provider_payload.payload.target_session == session },
    handler: ReminderInject({
      target: session,
      body: "Reviewer feedback: {{ event.provider_payload.payload.critique }}",
      tags: ["plan_feedback"],
      ttl_turns: 2,
    }),
  })

  agent_loop(task, "Revise the plan when reminders arrive. Re-emit plan.draft when revision is complete.", {session_id: session})
}

// --- reviewers ---
pipeline reviewer_setup() {
  trigger_register({
    id: "reviewer-on-draft",
    kind: "channel.emit",
    provider: "channel",
    match: {events: ["channel:plan.draft"]},
    handler: { event ->
      let draft = event.provider_payload.payload
      let critique = llm_call(
        "Critique this plan in 3 bullets:\n" + draft.plan,
        "You are a careful reviewer.",
      )
      emit_channel("plan.feedback", {
        target_session: draft.session_id,
        critique: critique,
        revision: draft.revision,
      })
    },
  })
}
```

Why channels over handoffs: both directions are 1-to-N — multiple
reviewers, multiple revision rounds — and neither side knows the
others' session ids ahead of time. The `target_session` field in the
feedback payload + a `when` predicate routes each critique back to the
right planner without a central registry.

Why `ReminderInject` for the inbound feedback: the planner is *already
running* in `agent_loop`. Spawning a new task would lose its
transcript; injecting a reminder lets it incorporate the critique on
the next turn.

## 4. Pipeline progress dashboard

Every step in every pipeline emits `pipeline.step.completed`; a
monitoring agent tenant-wide subscribes and maintains a live
dashboard. The producers don't know the monitor exists — they just
emit.

```harn,ignore
import { trigger_register } from "std/triggers"
import { register_step_hook } from "std/hooks"

// --- producers: any pipeline registering this hook gets free instrumentation ---
pipeline producer_setup() {
  register_step_hook({
    pattern: "*",
    post: { ctx ->
      emit_channel("pipeline.step.completed", {
        pipeline: ctx.pipeline_name,
        step: ctx.step_name,
        duration_ms: ctx.duration_ms,
        success: ctx.success,
      })
      return nil
    },
  })
}

// --- dashboard: one subscriber, scoped to the whole tenant ---
pipeline dashboard_setup() {
  trigger_register({
    id: "dashboard-on-step",
    kind: "channel.emit",
    provider: "channel",
    // Bare channel names default to tenant scope, so this catches
    // emits from any pipeline running for this tenant.
    match: {events: ["channel:pipeline.step.completed"]},
    handler: { event ->
      let row = event.provider_payload.payload
      dashboard_upsert(row.pipeline, row.step, {
        last_run_at: event.occurred_at,
        last_duration_ms: row.duration_ms,
        last_success: row.success,
      })
    },
  })
}
```

Why tenant scope: a bare `emit_channel("pipeline.step.completed",
...)` resolves to `tenant:<current>:pipeline.step.completed`. The
dashboard subscribes to the same name without prefix and automatically
receives emits from every pipeline running for that tenant. Cross-
tenant isolation is automatic — a sibling tenant's emits go to a
different topic.

Why a trigger over `event_log.subscribe`: triggers carry the
dispatcher's retry policy, DLQ routing, and replay receipts. If the
dashboard handler throws, the channel emit still lands cleanly on
`lifecycle.channel.audit`; the failed match goes to `trigger.dlq`.

## 5. Cross-pipeline coordination via drain handoff

Pipeline A processes work synchronously up to a watermark, then drains
deferred items by emitting a channel event. A nightly settlement
pipeline (B) subscribes and picks up the deferred work.

```harn,ignore
import { trigger_register } from "std/triggers"
import { pipeline_on_finish } from "std/lifecycle"

// --- pipeline A: live ingest ---
pipeline ingest_pipeline(events) {
  let deferred = []
  for event in events {
    let result = try_process_now(event)
    if result.deferred {
      deferred = deferred + [event]
    }
  }

  // On clean drain, hand the deferred bucket off to pipeline B.
  pipeline_on_finish({ ctx ->
    if ctx.status == "completed" && len(deferred) > 0 {
      emit_channel("pipeline.drained", {
        source_pipeline: "ingest",
        deferred_count: len(deferred),
        deferred_payload: deferred,
        drained_at: timestamp(),
      })
    }
  })
}

// --- pipeline B: nightly settlement ---
pipeline settlement_setup() {
  trigger_register({
    id: "settlement-on-drain",
    kind: "channel.emit",
    provider: "channel",
    match: {events: ["channel:pipeline.drained"]},
    when: { event -> event.provider_payload.payload.source_pipeline == "ingest" },
    handler: { event ->
      let bucket = event.provider_payload.payload.deferred_payload
      for deferred in bucket {
        settle(deferred)
      }
    },
  })
}
```

Why channels over a queue table: the `lifecycle.channel.audit` receipt
is the durable contract. Pipeline B doesn't need a polling loop or a
DB cursor; the dispatcher delivers exactly once per emit (subject to
the normal trigger retry policy), and the replay oracle can reproduce
the handoff across two runs.

Why `pipeline_on_finish` rather than emitting inline: a partial drain
on a failure mid-loop would publish an inconsistent bucket. Emitting
from the finish hook ensures pipeline A reached a clean terminal state
first.

## Picking the right primitive

For any of these recipes, the wrong tool is also worth knowing:

- **Don't use `channel(...) + send/receive`** (the in-process
  concurrency channel) for cross-pipeline or cross-process pub/sub —
  it lives only inside one VM. Reach for `emit_channel` whenever the
  publisher and subscriber could be different runs.
- **Don't use a webhook trigger for in-cluster events.** Webhook
  triggers carry HMAC verification and delivery-id dedupe that are
  pointless for trusted internal emits. Channels short-circuit both.
- **Don't use suspend/resume for "wake me when N things happen."** A
  suspended worker holds its transcript and process slot. Use a
  `batch` trigger + `ReminderInject` so the worker keeps running and
  only the reminder arrives.
