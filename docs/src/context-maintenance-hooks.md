# Context maintenance hook recipes

Long-lived coding hosts need context work that should not block the foreground
agent turn: fast index refreshes after file edits, slower librarian or
crystallization passes when the session is idle, and final cleanup when a
session ends. Harn's lifecycle hooks provide the scheduling points; hosts keep
ownership of the queue, worker process, storage, and undo semantics.

The portable rule is simple:

1. A hook handler decides whether work is needed and enqueues a host job.
2. The hook returns immediately with a context-maintenance receipt.
3. The worker updates the same receipt as `running`, `succeeded`, `failed`, or
   `skipped`.
4. Replay either includes the background job or emits a deterministic
   `skipped` receipt with the same dedupe key.

Use `std/context/maintenance` to build the shared receipt shape:

```harn
import { context_maintenance_queue_receipt } from "std/context/maintenance"

pub fn on_file_edited(event) {
  return context_maintenance_queue_receipt(
    "context.refresh",
    event,
    {retry_hint: {retryable: true, after_ms: 1000, max_attempts: 3}},
  )
}
```

## Receipt shape

`context_maintenance_receipt(job_id, status, input?)` returns
`harn.context_maintenance.job_receipt.v1`:

| Field | Purpose |
|---|---|
| `status` | One of `queued`, `running`, `succeeded`, `failed`, or `skipped` |
| `job_id`, `run_id`, `dedupe_key` | Stable job identity and replay lineage |
| `lifecycle_event` | Source hook, for example `FileEdited` or `PreCompact` |
| `affected_paths` | Workspace paths the job may inspect or refresh |
| `artifact_ids` | Host or Harn artifacts produced by the worker |
| `duration_ms` | Worker-observed duration; queued receipts use `0` |
| `retry_hint` | `{retryable, after_ms?, max_attempts?, reason?}` |
| `replay` | `{mode: "include" or "skip", determinism_key?, reason?}` |
| `error`, `metadata` | Optional failure detail and host-specific projection data |

The helper sorts and deduplicates paths and artifact ids before hashing the
dedupe key, so a replay fixture can compare receipts without depending on file
watch ordering.

## Hook recipes

| Lifecycle point | Recommended job | Why this hook |
|---|---|---|
| `file_edited` | `context.refresh` | Fast deterministic scan/index refresh for changed paths |
| `session_idle` | `context.crystallize` | Fuzzy or LLM-backed enrichment after foreground activity quiets |
| `pre_compact` | `context.crystallize` | Last chance to preserve durable facts before transcript compaction |
| `post_turn` | `context.refresh` or `context.crystallize` | React to tool outcomes while the model turn is fresh |
| `session_end` | `context.crystallize` | Final maintenance pass and artifact flush before teardown |

Hook handlers are advisory for these recipes. They should enqueue, emit a
receipt, and return quickly. If a host needs a foreground veto, use the
existing veto-capable events (`user_prompt_submit` and `pre_compact`) for the
policy decision and keep background maintenance in a separate job.

## Replay

Background jobs are replayed only when their output is part of the replay
contract. Deterministic scans, committed context-pack artifacts, and receipts
that an assertion inspects should use `replay.mode = "include"`.

Jobs that only warm host caches or refresh UI projections should use
`replay.mode = "skip"`. During replay, call
`context_maintenance_replay_decision(receipt, {mode: "skip"})`; it returns a
`skipped` receipt with the original `run_id` and `dedupe_key`, so downstream
views can see that the job was intentionally omitted rather than lost.

## Burin mapping

Burin's current two lanes map directly:

| Burin lane | Harn job id | Hook sources |
|---|---|---|
| `infra/refresh-context` | `context.refresh` | `file_edited`, `post_turn` |
| `infra/librarian-refresh` | `context.crystallize` | `session_idle`, `pre_compact`, `session_end` |

The Swift coordinator can keep its debounce/max-delay policy. The Harn package
only needs to emit the receipt shape above when it queues or completes work.

See `examples/triggers/context-maintenance/` for a copyable package with
manifest `[[hooks]]` entries and worker-pipeline stubs.
