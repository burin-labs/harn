# Pool cookbook

End-to-end patterns for agent pools. Each recipe is a self-contained,
copy-paste starting point. For the surface reference see
[Agent pools](../agent-pools.md); for the LLM-friendly quickref see the
"Agent pools" section in `docs/llm/harn-quickref.md`.

The recipes use `harn,ignore` fences because they wire up full
multi-pipeline topologies (producer, pool-draining pipeline, trigger
handlers). Each fragment type-checks; the orchestration loop assumes a
host that runs the constituent pipelines, such as `harn orchestrator`.

## Rate-limited webhook processor

Every webhook source posts to a single `channel:webhook.received`
channel. A pool with `max_concurrent: 10`, a per-source fair queue,
and a `ring_buffer(500)` overflow policy drains them. Bursty senders
cannot starve quieter ones; an unbounded spike drops the oldest
queued task (a `pool_drop` audit lands on `lifecycle.pool.audit`) but
never the actively-running ones.

```harn,ignore
import { trigger_register, SpawnToPool } from "std/triggers"
import { Backpressure, fair_round_robin, pool_create } from "std/lifecycle/pool"

pipeline webhook_intake_setup() {
  let bp = Backpressure()

  // One named pool, shared across every webhook source.
  pool_create({
    name: "webhook-work",
    max_concurrent: 10,
    queue: fair_round_robin("source"),
    backpressure: bp.ring_buffer(500),
    scope: "pipeline",
  })

  // Generic webhook connector emits `channel:webhook.received` for
  // every inbound payload. Route the channel into the pool.
  trigger_register({
    id: "webhook-router",
    kind: "channel.emit",
    provider: "channel",
    match: {events: ["channel:webhook.received"]},
    handler: SpawnToPool({
      pool: "webhook-work",
      key_from: "provider_payload.payload.source",
      priority_from: "provider_payload.payload.urgency",
      task_factory: { event ->
        let payload = event.provider_payload.payload
        return { -> process_webhook(payload) }
      },
    }),
  })
}

fn process_webhook(payload) {
  // Idempotent handler — see "Burst absorber for nightly batch jobs"
  // below for the durable-key pattern.
  return upsert_event(payload.source, payload.body)
}
```

Why a pool over per-source rate limiters: ten sources times one
limiter each is ten queues with no shared budget. A single pool with
`fair_round_robin("source")` enforces both the *global* cap (ten
workers, period) and *per-source* fairness in one primitive.

Why `ring_buffer(500)` over `block_submitter`: blocking the webhook
intake fiber stalls the entire connector. Dropping the oldest queued
task is the correct backpressure shape for an overloaded webhook
endpoint — the audit trail tells you who got dropped, and the
underlying retry policy of the sender re-delivers later.

Why `scope: "pipeline"`: the connector pipeline restarts on every
deploy. Pipeline scope reloads queued tasks from
`.harn/pools/<pipeline_id>__webhook-work.jsonl` so an in-flight burst
does not vanish on restart.

## GPU-routed inference pool

A multi-tenant inference service has four GPUs. Each inference call
must run on exactly one GPU; the pool's `max_concurrent` is sized to
the GPU count and the worker tier is pinned via the harn-cloud worker
selector. Tenants share the budget fairly, and any cross-tenant burst
queues rather than spilling onto non-GPU hosts.

```harn,ignore
import { trigger_register, SpawnToPool } from "std/triggers"
import { Backpressure, fair_round_robin, pool_create, pool_wait } from "std/lifecycle/pool"

pipeline inference_pool_setup() {
  let bp = Backpressure()
  let gpu_count = 4   // discovered from the host worker tier in real deployments

  pool_create({
    name: "gpu-inference",
    max_concurrent: gpu_count,
    queue: fair_round_robin("tenant_id"),
    backpressure: bp.queue(1000, "fail_submitter"),
    scope: "tenant",   // harn-cloud routes to the GPU-tier worker pool
  })

  trigger_register({
    id: "inference-router",
    kind: "channel.emit",
    provider: "channel",
    match: {events: ["channel:inference.requested"]},
    handler: SpawnToPool({
      pool: "gpu-inference",
      key_from: "provider_payload.payload.tenant_id",
      task_factory: { event ->
        let req = event.provider_payload.payload
        return { ->
          // Per-request closure; runs on a GPU-tier worker because the
          // pool's scope routes through the harn-cloud GPU worker tier.
          return run_gpu_inference(req.model, req.prompt, req.params)
        }
      },
    }),
  })
}

// Synchronous caller path that wraps the trigger flow for in-process use.
pipeline inference_call(tenant_id, model, prompt, params) {
  let pool = pool_get("gpu-inference")
  let handle = pool.submit({ ->
    return run_gpu_inference(model, prompt, params)
  }, {tenant_id: tenant_id})

  return pool_wait(handle).result
}
```

Why one pool over per-GPU pools: rebalancing across four pools when
one tenant is idle is your problem; rebalancing within one pool is
the runtime's. `max_concurrent: 4` plus `fair_round_robin` gives both
the hard cap and the dynamic balance for free.

Why `fail_submitter` over `block_submitter`: an inference request that
backs up for more than ~1000 deep is almost certainly going to time
out at the caller anyway. Failing fast with `HARN-POL-001` lets the
caller pick a fallback model or shed load deliberately.

Why `scope: "tenant"`: the pool routes through harn-cloud's GPU
worker tier (#306). Today this fails with a `host-routed (harn-cloud)
— not wired` diagnostic; you can run the same code with
`scope: "session"` for local development and flip the scope at deploy
time.

## Cross-customer fairness

A SaaS backend runs per-customer agent tasks (PR review, doc gen,
test triage). Without fairness, a single noisy customer's batch of
500 PRs starves every other customer for hours. `fair_round_robin`
on the customer id flips the worst case from "one customer drains
the queue" to "all active customers interleave one task at a time."

```harn,ignore
import { trigger_register, SpawnToPool } from "std/triggers"
import { Backpressure, fair_round_robin, pool_create } from "std/lifecycle/pool"

pipeline tenant_work_setup() {
  let bp = Backpressure()

  pool_create({
    name: "tenant-agents",
    max_concurrent: 20,
    queue: fair_round_robin("tenant_id"),
    backpressure: bp.queue(5000, "block_submitter"),
    scope: "pipeline",
  })

  // Direct submits from a connector loop.
  trigger_register({
    id: "tenant-agent-router",
    kind: "channel.emit",
    provider: "channel",
    match: {events: ["channel:agent_task.requested"]},
    handler: SpawnToPool({
      pool: "tenant-agents",
      key_from: "provider_payload.payload.tenant_id",
      priority_from: "provider_payload.payload.priority",
      task_factory: { event ->
        let req = event.provider_payload.payload
        return { -> run_agent_task(req.tenant_id, req.task_kind, req.input) }
      },
    }),
  })
}

fn run_agent_task(tenant_id, kind, input) {
  return agent_loop(input, "You are the " + kind + " agent.")
}
```

Tenant A submits 500 tasks, tenant B submits 10. Under FIFO, tenant
B's first task waits behind 500 of tenant A's; under
`fair_round_robin("tenant_id")`, tenant B's first task runs on the
*second* free slot. Once tenant B drains, tenant A keeps the full 20
workers to itself.

Why `block_submitter` over a drop policy: cross-customer fairness only
matters when *both* customers have work to submit. Blocking the
submitter at 5000 queued is a backstop against runaway producers
without losing any work.

Why `scope: "pipeline"`: the queue depth at restart can be tens of
thousands of tasks. Pipeline scope reloads them and resumes
draining; the per-task idempotency key from "Burst absorber" catches the
brief window where in-flight tasks are re-enqueued.

## Burst absorber for nightly batch jobs

A nightly cron triggers thousands of report-generation tasks. They
need to all *eventually* run, but only a few at a time to avoid
saturating a downstream SaaS API. A small `max_concurrent` plus a
large queue depth turns a "thundering herd" into a slow drain.

```harn,ignore
import { trigger_register, SpawnToPool } from "std/triggers"
import { Backpressure, fifo, pool_create, pool_wait } from "std/lifecycle/pool"

pipeline nightly_report_setup() {
  let bp = Backpressure()

  pool_create({
    name: "nightly-reports",
    max_concurrent: 3,                       // small drain rate
    queue: fifo(),                           // run in submission order
    backpressure: bp.queue(50000, "fail_submitter"),  // large absorber
    scope: "pipeline",
  })

  // Cron-triggered fan-out: one submit per customer report.
  trigger_register({
    id: "nightly-report-cron",
    kind: "cron",
    provider: "cron",
    match: {schedule: "0 2 * * *"},          // 02:00 UTC nightly
    handler: { event ->
      let pool = pool_get("nightly-reports")
      let customers = list_active_customers()
      for customer in customers {
        // Idempotency key = (run date, customer). A retry of the cron
        // for the same date short-circuits to the same task handle
        // instead of double-running.
        let key = event.occurred_at_date + ":" + customer.id
        pool.submit({ ->
          return generate_report(customer.id, event.occurred_at_date)
        }, {idempotency_key: key})
      }
    },
  })
}

fn generate_report(customer_id, run_date) {
  let data = fetch_customer_data(customer_id, run_date)   // slow SaaS call
  return upload_report(customer_id, run_date, data)
}
```

Why `fifo` over `priority`: every report has the same priority; ties
under `priority()` are FIFO anyway, but `fifo()` documents intent and
saves the priority comparator on every dequeue.

Why a 50000-deep queue: nightly cron can fan out tens of thousands of
tasks in a single burst. A small queue would force the cron handler
to either block or drop; either is the wrong shape for a once-a-night
job. The queue absorbs the burst and drains it across the rest of
the night at 3-at-a-time.

Why `idempotency_key`: the cron may retry if the orchestrator
restarts mid-handler. Without the key, every customer would get two
reports for the same date. With `(date, customer_id)` as the key,
the second submit returns the first task's handle and the closure
runs exactly once per night per customer.

Why `scope: "pipeline"`: the orchestrator can restart mid-night and
the remaining queue picks back up where it left off. Pipeline scope
plus idempotency keys is the canonical "at-least-once with
deduplication" pattern.
