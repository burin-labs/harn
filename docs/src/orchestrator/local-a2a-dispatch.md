# Local and A2A dispatch

Harn trigger handlers can move between in-process execution and A2A dispatch
without changing the handler's input contract. Keep the trigger `id`, provider
match, retry policy, and handler source stable; change only the handler target
when the boundary changes:

```toml
# local
handler = "handlers::triage_issue"

# federated
handler = "a2a://reviewer.example/triage"
```

The dispatcher still records the same core lifecycle shape:

- `dispatch_started`
- `attempt_recorded`
- `dispatch_succeeded`, `dispatch_failed`, `dispatch_waiting`, or `dlq_moved`
- a trust record for the terminal dispatch decision
- action-graph nodes and edges for replay comparison

## Choosing a mode

Use a local handler when the workflow should run inside the current Harn process:

- development and deterministic test fixtures
- low-latency handlers with no cross-tenant boundary
- handlers that need direct access to local host capabilities already granted to
  the current orchestrator

Use A2A dispatch when execution crosses an agent or trust boundary:

- a managed cloud worker owns the handler
- a federated agent has separate credentials, quotas, or receipts
- an operator needs to see that the work ran remotely
- retries and DLQ movement should treat the remote endpoint as a destination

## Trust and replay

Local and A2A dispatch share typed `TriggerEvent` input and JSON output
semantics. A2A wraps the event in a `harn.trigger.dispatch` envelope, sends it
with `message/send`, and unwraps the completed task's final text part as the
logical handler result.

The difference is recorded as metadata, not as a handler-source requirement.
Action-graph nodes and trust records include:

- `trust_boundary`: `local_process`, `federated_a2a`,
  `event_log_worker_queue`, or `persona_runtime`
- `execution_location`: `in_process`, `remote`, `queued`, or
  `managed_persona`
- `remote_identity`: the A2A target agent label for remote dispatch
- A2A task metadata such as `task_id`, `state`, `card_url`, `rpc_url`, and
  `remote_agent_id` when the remote card exposes it

Replay comparisons should assert the logical output and event shape while
allowlisting location-specific metadata such as `trust_boundary`,
`remote_identity`, remote URLs, and generated task ids.

## Failure modes

A2A dispatch fails closed for trust-boundary denials:

- cleartext HTTP endpoints require `allow_cleartext = true`
- agent-card authority mismatches are denied
- HTTP `401` / `403` dispatch responses are denied
- A2A `rejected` task state becomes a permission-denied dispatch failure

Remote timeouts are classified as timeout failures and remain retryable under
the trigger's retry policy. Incompatible A2A response schemas are protocol
failures and move to DLQ when retries are exhausted.

## Demo

The reference flow is in `examples/triggers/local-a2a-dispatch/`.

```sh
scripts/demo_local_a2a_dispatch.sh
```

The script starts a local `harn serve` A2A receiver on a random port, fires the
same deterministic issue event through a local handler and through A2A, and
prints whether the logical outputs match.
