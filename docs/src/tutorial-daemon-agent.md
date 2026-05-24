# Tutorial: from one-shot agent to durable daemon

This tutorial walks an agent through every rung of the lifecycle ladder: from
a single `agent_loop` call that returns once, to a parked worker that survives
process restart, to a bounded pool of agents that wake on channel events.
Each step builds on the previous one and runs unmodified on a fresh `harn`
install — no API keys, no extra setup, only the `mock` provider so the output
is deterministic.

If you want the long-form reference for any primitive used below, see
[Agent lifecycle](./agent-lifecycle.md), [Pipeline lifecycle](./pipeline-lifecycle.md),
[Pool stdlib](./stdlib/lifecycle-pool.md), and [Agent channels](./agent-channels.md).
The [lifecycle cookbook](./cookbooks/lifecycle.md) collects production-shaped
recipes that compose the same primitives.

> Each step prints a small diagnostic line ("status=...", "snapshot=...") so
> you can compare your run against the expected output. The mock provider
> prints a one-time hint about Ollama on first use; the tutorial output below
> elides it.

## 1. One-shot agent

Start with the smallest useful loop: one prompt, one mocked response, one
result. The pipeline returns as soon as the loop completes.

```harn
pipeline main() {
  llm_mock({text: "Triaged ticket #42 as duplicate of #38."})

  let result = agent_loop(
    "Triage ticket #42.",
    "You are a careful triage assistant.",
    {provider: "mock"},
  )

  log("status=" + result.status)
  log("text=" + result.visible_text)
}
```

```text
$ harn run step1.harn
status=done
text=Triaged ticket #42 as duplicate of #38.
```

This is the baseline. The loop ran one turn and returned `status: "done"`.
Nothing else is happening — no channels, no checkpoints, no resume.

## 2. Capture unsettled work at finish

Real pipelines spawn subagents, queue pool tasks, and emit channel events
that may outlive the body. The runtime exposes that work through
`harness.unsettled_state()`; `pipeline_on_finish` registers a callback that
fires after the pipeline returns and decides what to do with whatever is
still in flight. The `on_finish_drain` preset walks the unsettled buckets
and applies a default disposition (cancel, acknowledge, defer, drain) per
item.

```harn
import { on_finish_drain } from "std/lifecycle"

pipeline main() {
  pipeline_on_finish(on_finish_drain)
  llm_mock({text: "Triaged ticket #42 as duplicate of #38."})

  let result = agent_loop(
    "Triage ticket #42.",
    "You are a careful triage assistant.",
    {provider: "mock"},
  )

  log("status=" + result.status)
  log("text=" + result.visible_text)
}
```

The visible output matches step 1 — the loop returns naturally, so there is
nothing unsettled to act on. The registration becomes load-bearing in step
3, where a parked worker stays in `suspended_subagents` until the drain
callback walks the bucket. See [Pipeline lifecycle presets](./stdlib/lifecycle.md)
for the rest of the preset family (`on_finish_abandon`,
`on_finish_handoff_to`, `on_finish_block_until_settled`) and
[Pipeline lifecycle](./pipeline-lifecycle.md) for the full callback
contract.

## 3. Self-park mid-loop

`agent_loop` exposes `agent_await_resumption` to the model as a callable
tool. When the model wants to wait on an external signal (a human review, an
upstream merge, anything off-VM), it calls that tool and the loop yields
between turns. The pipeline gets back `status: "suspended"` with a snapshot
path.

```harn
import { on_finish_drain } from "std/lifecycle"

pipeline main() {
  pipeline_on_finish(on_finish_drain)

  llm_mock({
    tool_calls: [{
      id: "park_1",
      name: "agent_await_resumption",
      arguments: {reason: "waiting on maintainer review"},
    }],
  })

  let result = agent_loop(
    "Triage ticket #42, escalate to a human if you need review.",
    "If you need a human review before proceeding, call agent_await_resumption.",
    {provider: "mock", tool_format: "native", max_iterations: 2},
  )

  log("status=" + result.status)
  log("reason=" + result.reason)
  log("snapshot=" + result.handle.snapshot_path)
}
```

```text
$ harn run step3.harn
status=suspended
reason=waiting on maintainer review
snapshot=.harn/workers/worker_019e57c5-….json
```

The snapshot is a JSON document on disk. The runtime persists the full
transcript, the parsed conditions, the resume responsibility, and enough
session metadata to rehydrate in a different process. Suspend is
*cooperative* — the loop honors the request at the next turn boundary, not
mid-tool-call. See [Agent lifecycle § When to suspend](./agent-lifecycle.md#when-to-suspend)
for the rest of the ways an agent can yield and which one to reach for.

## 4. Resume from the snapshot

The snapshot from step 3 is enough to drive the worker forward in any
process. The CLI does this with `harn run --resume <snapshot>`: it
rehydrates the worker, replays a single-shot `resume_continuity` system
reminder onto the next turn, and finishes the loop. The script below
runs both halves in one process so the tutorial stays self-contained;
the prose after the snippet shows the cross-process command.

```harn
import { on_finish_drain } from "std/lifecycle"
import { resume_agent, wait_agent } from "std/agent/workers"

pipeline main() {
  pipeline_on_finish(on_finish_drain)

  llm_mock({
    tool_calls: [{
      id: "park_1",
      name: "agent_await_resumption",
      arguments: {reason: "waiting on maintainer review"},
    }],
  })
  llm_mock({text: "Approved. Triaged ticket #42 as duplicate of #38."})

  let first = agent_loop(
    "Triage ticket #42, escalate to a human if you need review.",
    "If you need a human review before proceeding, call agent_await_resumption.",
    {provider: "mock", tool_format: "native", max_iterations: 3},
  )

  log("first.status=" + first.status)
  log("snapshot=" + first.handle.snapshot_path)

  resume_agent(first.handle)
  let done = wait_agent(first.handle)

  log("after_resume=" + done.status)
  log("text=" + done.result.summary)
}
```

```text
$ harn run step4.harn
first.status=suspended
snapshot=.harn/workers/worker_019e57c9-….json
after_resume=completed
text=Approved. Triaged ticket #42 as duplicate of #38.
```

To do the same thing across processes, run step 3, copy the printed snapshot
path, and run `harn run --resume <path> --json`. The `--json` flag dumps the
final loop value as a structured envelope so it can be piped into another
tool. Snapshots live under `.harn/workers/` by default; the path is
script-relative, so resume the script from the same working directory.

## 5. Wake on a channel event

Step 3 left the worker parked open — only an operator can resume it. Most
real waits have a concrete signal to listen for: a PR merging, a release
cutting, a calendar event firing. Attach `conditions.trigger` to the
`agent_await_resumption` call and the runtime registers the trigger with
the dispatcher; firing the trigger drives the worker to completion with
`initiator: "triggered"` and no explicit `resume_agent` call from the
pipeline.

```harn
import { on_finish_drain } from "std/lifecycle"

pipeline main() {
  pipeline_on_finish(on_finish_drain)

  llm_mock({
    tool_calls: [{
      id: "park_for_release",
      name: "agent_await_resumption",
      arguments: {
        reason: "waiting on release.cut",
        conditions: {
          trigger: {
            kind: "channel.emit",
            provider: "channel",
            match: {events: ["channel:release.cut"]},
          },
        },
      },
    }],
  })
  llm_mock({text: "Release cut; tagged v0.9.0 and posted to the changelog."})

  let worker = sub_agent_run(
    "Tag the next release once the maintainer signals.",
    {provider: "mock", background: true, tool_format: "native", max_iterations: 3},
  )

  let parked = wait_agent(worker)
  log("parked_status=" + parked.status)
  log("waiting_on=" + parked.suspension.conditions.trigger.match.events[0])

  emit_channel("release.cut", {tag: "v0.9.0"})

  let done = wait_agent(worker)
  log("final_status=" + done.status)
  log("final_text=" + done.result.summary)
}
```

```text
$ harn run step5.harn
parked_status=suspended
waiting_on=channel:release.cut
final_status=completed
final_text=Release cut; tagged v0.9.0 and posted to the changelog.
```

`wait_agent` blocks until the worker reaches a terminal *or* parked state,
which gives the pipeline a deterministic point to fire the event. Any
trigger kind that `trigger_register` accepts works as a resume condition —
GitHub webhooks, file watchers, calendar events, custom providers. See
[Agent channels](./agent-channels.md) for the channel surface and
[Agent lifecycle § Conditioned resume](./agent-lifecycle.md#conditioned-resume)
for the rest of the `ResumeConditions` shape (timeouts, `on_event`,
`resume_by`).

## 6. Fan out under a bounded pool

A real product runs many agents at once but only so many in parallel. The
`std/lifecycle/pool` registry shares a single concurrency budget across
submissions; `max_concurrent` caps the active slot count, the rest queue.
Pool tasks compose with the same waiter as agent handles, so `pool_wait`
(or `wait_agent`) collects results uniformly.

```harn
import { on_finish_drain } from "std/lifecycle"
import { pool_create, pool_wait } from "std/lifecycle/pool"

pipeline main() {
  pipeline_on_finish(on_finish_drain)

  let tickets = ["t-101", "t-102", "t-103"]

  for ticket in tickets {
    llm_mock({
      match: "*" + ticket + "*",
      text: "Triaged " + ticket + " as duplicate of t-100.",
      consume_match: true,
    })
  }

  let pool = pool_create({name: "ticket-triage", max_concurrent: 2})
  var handles = []
  for ticket in tickets {
    let t = ticket
    handles = handles + [pool.submit({ ->
      return agent_loop(
        "Triage ticket " + t + ".",
        "You are a careful triage assistant.",
        {provider: "mock"},
      )
    })]
  }

  let outcomes = pool_wait(handles)
  for outcome in outcomes {
    log(outcome.status + " " + outcome.result.visible_text)
  }
}
```

```text
$ harn run step6.harn
completed Triaged t-101 as duplicate of t-100.
completed Triaged t-102 as duplicate of t-100.
completed Triaged t-103 as duplicate of t-100.
```

The pool ran two agents concurrently and queued the third. Combine the
pool with the parking pattern from step 5 to fan a queue of long-running
human-in-the-loop reviews across a bounded worker set — each pool task
spawns an agent that parks waiting for its own signal, the orchestrator
emits the signals as approvals land, and the resume-continuity reminder
(auto-injected on every resume; visible in the persisted transcript) tells
the model what changed during the pause so it picks up without re-reading
the whole conversation.

See [Pool stdlib](./stdlib/lifecycle-pool.md) for queue strategies,
backpressure, and fairness keys; the
[Pool cookbook](./cookbooks/pools.md) shows the production-shaped patterns.

## Where to go next

- [Agent lifecycle](./agent-lifecycle.md) — the long-form reference for
  every suspend/resume primitive used above, plus `ResumeBy.*` for naming
  who owns the resume.
- [Pipeline lifecycle cookbook](./cookbooks/lifecycle.md) — multi-pipeline
  hand-off, custom drain policies, supervised suspend denial, replay-
  deterministic test harnesses.
- [Daemon stdlib](./stdlib/daemon.md) — first-class `daemon_spawn` /
  `daemon_trigger` / `daemon_resume` wrappers when a worker needs to outlive
  a single pipeline run.
- [`harn run --resume` in the CLI reference](./cli-reference.md) — the
  out-of-process resume path used in step 4.
