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
