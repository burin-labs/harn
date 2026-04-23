# Agent State

`std/agent_state` is Harn's durable, session-scoped scratch space for
agent orchestration. It gives a caller-owned root directory plus a
session id a small set of predictable operations:

- write text blobs atomically
- read them back later
- list keys deterministically
- delete keys
- persist a machine-readable handoff document
- reopen the same session from a later process with `agent_state_resume`

The important design point is that the primitive is **generic**. Harn
owns the durable-state substrate; host apps own their schema and naming
conventions layered on top of it.

## Import

```harn
import "std/agent_state"
```

## Functions

| Function | Returns | Notes |
|---|---|---|
| `agent_state_init(root, options?)` | `state_handle` | Creates or reopens a session-scoped state root under `root/<session_id>/` |
| `agent_state_resume(root, session_id, options?)` | `state_handle` | Reopens an existing session; errors if it does not exist |
| `agent_state_write(handle, key, content)` | `nil` | Atomic temp-write plus rename |
| `agent_state_read(handle, key)` | `string` or `nil` | Returns `nil` for missing keys |
| `agent_state_list(handle)` | `list<string>` | Lexicographically sorted, recursive, deterministic |
| `agent_state_delete(handle, key)` | `nil` | Missing keys are ignored |
| `agent_state_handoff(handle, summary)` | `nil` | Writes a structured JSON handoff envelope to `__handoff.json` |
| `agent_state_handoff_key()` | `string` | Returns the reserved handoff key name (`"__handoff.json"`) |

## Handle shape

`agent_state_init(...)` and `agent_state_resume(...)` return a tagged
dict:

```harn
{
  _type: "state_handle",
  backend: "filesystem",
  root: "/absolute/root",
  session_id: "session-123",
  handoff_key: "__handoff.json",
  conflict_policy: "ignore",
  writer: {
    writer_id: "worker-a",
    stage_id: "worker-a",
    session_id: "session-123",
    worker_id: "worker-a"
  }
}
```

The exact fields are stable on purpose. Other runtime features can build
on the same handle semantics without introducing a second durable-state
model.

## Session ids

`agent_state_init(root, options?)` looks for `options.session_id` first.
If it is absent, Harn defaults to the active agent/workflow session id
when one exists. Outside an active agent context, Harn mints a fresh
UUIDv7.

That means common agent code can usually say:

```harn
import "std/agent_state"

pipeline default() {
  let state = agent_state_init(".harn/state", {writer_id: "planner"})
  agent_state_write(state, "plan.md", "# Plan")
}
```

and get a session-specific namespace automatically.

## Keys and layout

Keys are always **relative** to the session root. Nested paths are fine:

```harn
import "std/agent_state"

pipeline default() {
  let state = agent_state_init(".harn/state", {writer_id: "planner"})
  agent_state_write(state, "plan.md", "# Plan")
  agent_state_write(state, "evidence/files.json", "{\"paths\":[]}")
}
```

Rejected key forms:

- absolute paths
- any path containing `..`
- reserved internal metadata paths

The default filesystem backend stores user content under:

```text
<root>/<session_id>/<key>
```

with internal writer metadata stored separately under a hidden backend
directory. `agent_state_list(...)` only returns user-visible keys.

## Atomic writes

`agent_state_write(...)` writes to a temp file in the target directory,
syncs it, then renames it into place. If the process crashes before the
rename, the old file remains intact and the partially-written temp file
never becomes the visible key.

This guarantees "no partial file at the target path", which is the
durability property the primitive is designed to expose.

## Handoff documents

`agent_state_handoff(handle, summary)` stores a JSON envelope at
`__handoff.json`:

```json
{
  "_type": "agent_state_handoff",
  "version": 1,
  "session_id": "session-123",
  "key": "__handoff.json",
  "handoff": {
    "_type": "handoff_artifact",
    "source_persona": "merge_captain",
    "target_persona_or_human": {
      "kind": "human",
      "label": "maintainer"
    },
    "task": "Approve the rollout window",
    "reason": "A human must authorize production side effects"
  },
  "summary": {
    "status": "ready"
  }
}
```

Callers still own the shape of `summary`, but durable personas should prefer a
typed handoff payload over transcript dumps or vague prose. When the payload
matches the typed handoff shape, Harn preserves it under the `handoff` field so
receivers can load structured task, evidence, open-question, and side-effect
context without importing the source transcript.

## Two-writer discipline

Each handle can carry a writer identity and conflict policy:

```harn
let state = agent_state_init(".harn/state", {
  session_id: "demo",
  writer_id: "planner",
  conflict_policy: "error"
})
```

Supported policies:

- `"ignore"`: accept overlapping writes silently
- `"warn"`: accept the write and emit a runtime warning
- `"error"`: reject the write before replacing the existing content

Conflict detection compares the previous writer id for that key with the
current writer id. This is intentionally simple and deterministic: it is
a guard rail against accidental stage overlap, not a full distributed
locking protocol.

## Backend seam

The default implementation is a filesystem backend, but the storage
layer is split behind a backend trait in
`crates/harn-vm/src/stdlib/agent_state/backend.rs`.

That trait is designed around:

- scope creation/resume
- atomic blob read/write/delete
- deterministic list
- conflict metadata on write

so future backends such as in-memory, SQLite, or remote stores can plug
in without changing the Harn-facing handle semantics.

## Example

```harn
import "std/agent_state"

pipeline default() {
  let state = agent_state_init(".harn/state", {
    session_id: "review-42",
    writer_id: "triage"
  })

  agent_state_write(state, "plan.md", "# Plan\n- inspect PR")
  agent_state_handoff(state, {
    status: "needs_review",
    next_stage: "implement"
  })

  let resumed = agent_state_resume(".harn/state", "review-42", {
    writer_id: "implement"
  })
  println(agent_state_read(resumed, "plan.md"))
}
```
