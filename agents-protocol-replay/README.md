# Harn Agents Protocol Replay Contract

This directory is the canonical v1 replay-as-API contract for the Harn Agents
Protocol. It makes Harn's durable EventLog replay model visible as a public API
verb without exposing private runtime internals.

Until a standalone specification site exists, the public URL for this artifact
is:

<https://github.com/burin-labs/harn/tree/main/agents-protocol-replay>

## REST entrypoint

Task replay is created with:

```text
POST /v1/tasks/{task_id}/replay
```

The response is a newly accepted Task. The replay Task MUST set
`parent_task_id` to the source Task id, use the same Session and Workspace
unless policy requires an isolated Branch, and emit normal Task lifecycle
events for the replay run.

The request body is optional:

```json
{
  "mode": "with_overrides",
  "override": {
    "llm:main:1": {
      "kind": "llm_provider_response",
      "event_id": "event_01JZ7001",
      "value": {
        "id": "chatcmpl_fixture_1",
        "choices": [
          {
            "message": {
              "role": "assistant",
              "content": "Done."
            }
          }
        ]
      },
      "sha256": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "visibility": "receipt_only"
    }
  }
}
```

## Modes

- `exact`: replay only recorded EventLog material. The server MUST fail instead
  of refreshing unavailable model responses, tool returns, secrets, time, or
  host facts.
- `with_overrides`: replay recorded EventLog material, but replace named
  nondeterministic dependencies with the `override` map.
- `from_checkpoint`: restore a recorded checkpoint boundary, then replay later
  EventLog entries. The request MUST include `checkpoint_id` or
  `checkpoint_event_id`.

## Override keys

Override keys are stable implementation-facing dependency labels. They SHOULD
be deterministic across conforming implementations for the same module and
input. Recommended forms are:

- `llm:<call_id>` for provider responses.
- `mcp:<server>:<tool_call_id>` for MCP tool returns.
- `secret:<name>` for secret material supplied by the Harness.
- `time:<label>` for wall-clock or monotonic clock reads.
- `host:<capability>:<call_id>` for host facts or tool results.

Override values MAY inline JSON in `value` or reference a durable Artifact via
`artifact_id`. Secret overrides SHOULD use `visibility: receipt_only`.

## EventLog use

Replay MUST read the source Task's EventLog entries in event order. Replayed
events SHOULD preserve original event ids when the transport can safely do so.
When ids are remapped, events MUST include replay metadata with:

- `source_task_id`
- `replay_task_id`
- `original_event_id`
- `replay_cursor`
- `mode`
- `override_key` when a substitution produced the event

## Receipt deltas

Every applied override MUST be recorded as a Receipt delta. A delta identifies
the original event or material path, the override key, before/after hashes when
available, and the reason supplied by the caller or Harness policy.

Replay Receipts MUST NOT expose hidden chain-of-thought or secret values.
Receipt-only substitutions should record hashes and artifact references rather
than raw material.

## Determinism fixture

The conformance fixture in `fixtures/valid/` defines the minimum byte-stable
case:

- `task-replay-request.json`: a replay request with deterministic overrides.
- `replay-source.harn`: the module both implementations execute.
- `task-replay-receipt.json`: the canonical Receipt shape expected after replay.

Two conforming implementations that run the same module with the same EventLog
history and the same overrides MUST produce byte-identical canonical Receipt
JSON after RFC 8785 canonicalization, excluding signatures that are explicitly
implementation-specific.
