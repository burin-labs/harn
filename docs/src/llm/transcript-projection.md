# Transcript projection

Transcript projection is the read-side dual of compaction. Compaction
**archives** historical messages forever; projection picks which slice of the
**unchanged** raw transcript the next provider request will see. Both compose:
compaction rewrites the persisted message list, projection then chooses what to
expose on the next turn — without ever rewriting the immutable audit trail.

Pair this page with [Streaming and transcripts](./streaming.md#transcript-management)
for the underlying transcript model and [`agent_loop`](./agent_loop.md) for the
per-turn integration.

## Why projection

A failed tool call that the agent corrects on the next turn is valuable in the
audit log and noisy in the model context window. Today scripts that want a
clean prefix either keep raw history (cheap context, expensive tokens) or
hand-rewrite messages (cheap tokens, lost audit lineage). Projection moves the
choice into the runtime: raw events stay frozen, the model sees a clean view,
and a `transcript.projection` event records the exact decision for replay.

## `transcript_project(transcript, options?)`

A pure builtin: given an immutable transcript, returns a projected view plus
metadata, without mutating the input. Use it directly when you want a
projection without persisting the event (for example, to preview a
clean view in a UI).

```harn
let view = transcript_project(transcript, {policy: "clean_tool_repair"})
log(view.policy)          // "clean_tool_repair"
log(view.kept_count)      // messages surviving
log(view.dropped_count)   // messages hidden from the next request
log(view.prefix_hash)     // "sha256:..." of the projected prefix
log(view.event.kind)      // "transcript.projection" — ready to append to a session
log(view.messages)        // the model-visible prefix
log(view.provider_safety_blocked) // true when a signed reasoning block was
                                  // protected from removal
```

`options` accepts either a string shorthand (`"clean_tool_repair"`) or a dict:

| Field | Default | Meaning |
|-------|---------|---------|
| `policy` | `"raw"` | One of the policies below. |
| `respect_provider_signatures` | `true` | Refuse to drop messages with a signed `thinking` block (Anthropic) and fall back to raw. |
| `reason` | derived | Override the human-readable reason recorded on the projection event. |
| `keep_last` | `0` | `summary_prefix` only — number of trailing messages kept verbatim. |
| `summary` | `transcript.summary` | `summary_prefix` only — synthetic summary message body. |
| `projector` | _required for `custom`_ | Closure receiving `messages: list`, returning either a list of projected messages or `{messages, reason?, kept_indices?, dropped_indices?}`. |

## Built-in policies

### `raw`

Identity projection. Always safe; recorded with `reason = "raw_passthrough"`.
Use it to bind a deterministic prefix hash to a turn without changing what the
model sees.

### `clean_tool_repair`

For every tool that **later succeeded**, hide the earlier failed
`(assistant_call, tool_error_result)` pair from the next prefix. The audit log
keeps both turns; the model sees the corrected call only. Provider signatures
on the failed turn block the drop (see [Provider safety](#provider-safety)).

### `squash_failed_calls`

Hide assistant turns whose **only** observable outcome was a failed tool call
(and the matching error result). Use this when the agent's failed-then-recovered
chains aren't worth even a one-line acknowledgement on the next turn.

### `summary_prefix`

Replace the prefix before `keep_last` trailing messages with a single synthetic
`system` message carrying a rollup summary. The synthetic message is flagged
with `_harn_projection.synthetic = true` so observability sinks can render it
distinctly from the original transcript events.

```harn
let view = transcript_project(transcript, {
  policy: "summary_prefix",
  keep_last: 3,
  summary: "Earlier turns: investigated repo layout and ran tests.",
})
```

### `custom`

Pass a closure to compose your own logic. Indices are recovered by matching
the returned messages against the raw prefix; you can override that mapping
explicitly by returning `{messages, kept_indices, dropped_indices}`.

```harn
let view = transcript_project(transcript, {
  policy: "custom",
  projector: fn(messages) {
    // Drop tool error results carrying full stack traces.
    var kept = []
    var dropped = []
    for (idx, msg) in iter(messages).enumerate() {
      if msg.role == "tool" && msg.content.starts_with("Traceback") {
        dropped = dropped.push(idx)
      } else {
        kept = kept.push(msg)
      }
    }
    return {messages: kept, reason: "traceback_squashed"}
  },
})
```

## Composition with `agent_loop`

Pass `transcript_projection` in agent options to apply a policy on every turn.
The loop calls `agent_session_project_turn` before each provider request,
appends the resulting `transcript.projection` event to the raw transcript, and
emits a typed `TranscriptProjected` agent event for hosts.

```harn,ignore
let result = agent_loop(
  "Fix the failing tests.",
  "You are a test repair agent.",
  {
    tools: dev_tools(),
    transcript_projection: {policy: "clean_tool_repair"},
  },
)
let events = transcript_events_by_kind(result.transcript, "transcript.projection")
log(len(events))                                  // one per turn
log(events[0].metadata.policy)                    // "clean_tool_repair"
log(events[0].metadata.prefix_hash)               // "sha256:..."
log(events[0].metadata.kept_indices)              // indices kept from raw messages
log(events[0].metadata.dropped_indices)           // indices hidden from the prefix
log(events[0].metadata.provider_safety_blocked)   // signed-reasoning guardrail state
```

Projection composes with compaction: compaction rewrites the persistent
message list first; projection runs on top of the (already-compacted)
transcript so its `kept_indices` reference whatever messages remain. Both
emit independent transcript events so replay can reconstruct the full
lineage.

## Provider safety

Anthropic Sonnet/Opus models can emit `thinking` content blocks with an opaque
`signature` proving the block has not been tampered with. Stripping such a
message from the prefix would invalidate the signature on the next turn, so
projection refuses by default: the projection result falls back to raw, sets
`provider_safety_blocked: true`, and the recorded event explains the conflict.

If you're using projection for local-only preview (for example, rendering a
clean view in a UI without re-sending it to the provider), pass
`respect_provider_signatures: false` to opt out.

## Host integration

The same metadata reaches hosts two ways:

- **Persisted** as a `transcript.projection` event in the raw transcript —
  visible to `transcript_events_by_kind`, replay, and any consumer reading the
  audit log.
- **Live** as the typed `TranscriptProjected` agent event, surfaced over ACP as
  a `transcript_projected` `sessionUpdate` carrying `_meta.harn` fields:
  `policy`, `reason`, `prefixHash`, `keptCount`, `droppedCount`, and
  `providerSafetyBlocked`. Burin Code and other hosts use this to render a
  raw vs. projected side-by-side view without re-parsing the transcript.

Replay can reconstruct both views deterministically: the raw events are
immutable, and applying the same policy against the same raw prefix produces
the same `prefix_hash`.
