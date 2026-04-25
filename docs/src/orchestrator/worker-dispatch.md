# Worker Dispatch

`worker://<queue>` is Harn's durable queue-delegation path for triggers. Unlike
`a2a://...`, it does not call a specific remote agent immediately. The
dispatcher appends a job to the shared EventLog and returns an enqueue receipt;
some other orchestrator or handler-only consumer drains the queue later.

## When To Use It

Use `worker://...` when you want:

- a named queue instead of a fixed remote endpoint
- first-to-claim load balancing across multiple consumers
- crash-safe handoff backed by the EventLog
- producer and consumer roles to run from different manifests against the same
  state backend

Use `a2a://...` when you want RPC-style delivery to a specific remote agent.

## Queue Model

Each queue uses four append-only topic families:

- `worker.queues`: queue catalog / discovery
- `worker.<queue>`: enqueued jobs
- `worker.<queue>.claims`: claim, renewal, ack, release, and purge records
- `worker.<queue>.responses`: handler outcomes recorded by queue drains

Jobs are delivered with at-least-once semantics:

1. Producer dispatch enqueues a `trigger_dispatch` job record.
2. A consumer claims the next ready job with a TTL.
3. The consumer renews the claim while it is working.
4. Successful jobs are acked and leave the ready pool.
5. Failed or crashed consumers leave the job unacked; once the claim TTL
   expires, another consumer can reclaim it.

Queue priority supports `high`, `normal`, and `low`. Consumers drain high first,
and older normal jobs are promoted ahead of newer high-priority work after
15 minutes to prevent starvation.

## Producer / Consumer Split

The simplest operating model is two manifests sharing one state dir:

Producer manifest:

```toml
[[triggers]]
id = "incoming-review-task"
kind = "a2a-push"
provider = "a2a-push"
match = { events = ["a2a.task.received"] }
handler = "worker://triage"
priority = "high"
```

Consumer manifest:

```toml
[exports]
handlers = "lib.harn"

[[triggers]]
id = "incoming-review-task"
kind = "a2a-push"
provider = "a2a-push"
match = { events = ["a2a.task.received"] }
handler = "handlers::on_task"
```

The consumer uses the same trigger id so `harn orchestrator queue drain` can
resolve the queued job back to a concrete local handler.

## CLI Workflow

Inspect queues:

```bash
harn orchestrator queue \
  --config consumer/harn.toml \
  --state-dir ./state \
  ls
```

Drain one queue manually:

```bash
harn orchestrator queue \
  --config consumer/harn.toml \
  --state-dir ./state \
  drain triage \
  --consumer-id worker-a
```

Important behavior:

- successful jobs append a response record and are acked
- failed jobs append a failure response record but are left unacked, so they can
  be reclaimed after the claim TTL expires
- `--claim-ttl` defaults to 5 minutes and controls how long an in-flight claim
  stays reserved before another consumer can take it

Purge ready jobs only:

```bash
harn orchestrator queue \
  --config consumer/harn.toml \
  --state-dir ./state \
  purge triage \
  --confirm
```

## Backend Scope

Worker queues inherit the scope of the configured EventLog backend:

- SQLite or file backends: practical within-host sharing through the same state
  directory
- future shared backends: multi-host queue sharing without changing the trigger
  manifest shape

That keeps the product model simple: the same manifest-level `worker://queue`
shape works for local single-host deployments today and shared-log deployments
later.

## Fair-Share Scheduler

When a single worker queue is shared across multiple tenants, bindings, or
trigger ids, the dispatcher's per-binding flow-control gates do not on their
own prevent one hot stream from monopolising claim capacity. The fair-share
scheduler sits in front of `claim_next` and rotates across a configurable
fairness key so a cold tenant or binding always makes progress.

The default policy is **FIFO** — single-tenant deployments behave exactly as
before unless they opt in.

### Configuring the policy

The active policy is read from environment variables when the queue is
constructed (process start, drain command, etc.):

| Variable | Default | Notes |
| --- | --- | --- |
| `HARN_SCHEDULER_STRATEGY` | `fifo` | `fifo` or `drr`. |
| `HARN_SCHEDULER_FAIRNESS_KEY` | `tenant` | `tenant`, `binding`, `trigger-id`, or `tenant-and-binding`. |
| `HARN_SCHEDULER_QUANTUM` | `1` | Credits granted per round per unit weight (DRR only). |
| `HARN_SCHEDULER_STARVATION_AGE_MS` | `300000` | Promote any ready job older than this (ms). `0` disables. |
| `HARN_SCHEDULER_MAX_CONCURRENT_PER_KEY` | `0` | Cap on in-flight claims per fairness key. `0` = unlimited. |
| `HARN_SCHEDULER_DEFAULT_WEIGHT` | `1` | Weight applied to keys not listed in `HARN_SCHEDULER_WEIGHTS`. |
| `HARN_SCHEDULER_WEIGHTS` | _(empty)_ | Comma-separated `key:weight` (e.g. `tenant-a:3,tenant-b:1`). |

Example: enable tenant fair-share with a 3:1 weight ratio between two tenants
and a one-minute starvation promotion threshold.

```bash
export HARN_SCHEDULER_STRATEGY=drr
export HARN_SCHEDULER_FAIRNESS_KEY=tenant
export HARN_SCHEDULER_WEIGHTS=tenant-a:3,tenant-b:1
export HARN_SCHEDULER_STARVATION_AGE_MS=60000
harn orchestrator queue ls --json
```

Existing per-binding flow-control gates (`max_concurrent`, throttle, debounce,
batch, singleton) still apply after a job is selected. The scheduler decides
_who gets a turn first_, not whether the gate ultimately admits the dispatch.

### Inspecting fairness state

`harn orchestrator queue ls --json` now includes a `scheduler` block:

```json
{
  "scheduler": {
    "policy": {
      "strategy": { "kind": "deficit-round-robin", "quantum": 1, "starvation_age_ms": 300000 },
      "fairness_key": "tenant",
      "weights": { "tenant-a": 3, "tenant-b": 1 },
      "default_weight": 1,
      "max_concurrent_per_key": 0
    },
    "per_queue": [
      {
        "queue": "triage",
        "strategy": "drr",
        "fairness_key": "tenant",
        "rounds_completed": 7,
        "starvation_promotions_total": 0,
        "keys": [
          {
            "fairness_key": "tenant-a",
            "weight": 3,
            "deficit": 2,
            "in_flight": 1,
            "selected_total": 14,
            "deferred_total": 0,
            "ready_jobs": 6,
            "oldest_ready_age_ms": 1240
          },
          {
            "fairness_key": "tenant-b",
            "weight": 1,
            "deficit": 0,
            "in_flight": 0,
            "selected_total": 4,
            "deferred_total": 0,
            "ready_jobs": 0,
            "oldest_ready_age_ms": 0
          }
        ]
      }
    ]
  }
}
```

The plain-text `ls` output also surfaces the per-key state for at-a-glance
operator debugging.

### Metrics

The scheduler emits four Prometheus families (visible via `harn orchestrator
metrics`):

- `harn_scheduler_selections_total{queue,fairness_dimension,fairness_key}`
- `harn_scheduler_deferrals_total{queue,fairness_dimension,fairness_key}`
- `harn_scheduler_starvation_promotions_total{queue,fairness_dimension,fairness_key}`
- `harn_scheduler_deficit{queue,fairness_dimension,fairness_key}` (gauge)
- `harn_scheduler_oldest_eligible_age_seconds{queue,fairness_dimension,fairness_key}` (gauge)

Together they let dashboards show effective share, deferred claims, and the
oldest-eligible age per fairness key.

### Algorithm summary

The DRR strategy implements weighted round robin with deficit accumulation:

1. Group ready candidates by the configured fairness key.
2. If any candidate's age exceeds `starvation_age_ms`, promote the oldest
   eligible job regardless of credit balance.
3. Otherwise, scan eligible keys in round-robin order starting after the
   previously selected key. The first key with a positive credit balance wins.
4. When no key has credits, refill every eligible key with `weight × quantum`
   credits (one full round) and rescan.
5. Selecting a job decrements the chosen key's credits by one.

Because deficits are self-correcting, the scheduler keeps in-memory state per
queue rather than persisting it to the event log — restarts are cheap and
quickly re-stabilise to the configured weight ratios.
