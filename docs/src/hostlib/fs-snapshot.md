# Per-tool-call filesystem snapshots

`harn-hostlib` ships a Gemini-style `/restore` primitive paralleling the
[staged filesystem mode](staged-fs.md): a snapshot captures the
pre-image of paths a single mutating tool call is about to touch, so a
client can surgically roll the change back without affecting untracked
work.

The host adapter registers the implementation as part of the `fs` capability
family. Scripts use the `HarnessFs` interface:

| Method | Result |
|--------|--------|
| `harness.fs.snapshot(request)` | Register (and optionally pre-capture paths for) a snapshot keyed by `scope_id`—canonically the ACP `toolCallId`. |
| `harness.fs.restore(request)` | Write the captured pre-image back onto disk; delete paths the snapshot saw as absent. |
| `harness.fs.list_snapshots(request)` | List the snapshots registered for a session, sorted by capture time. |
| `harness.fs.drop_snapshot(request)` | Remove a snapshot's in-memory and on-disk state. |

## Capture modes

**Explicit.** Pass a `paths` list to `harness.fs.snapshot(...)`; the bytes
are copied into the snapshot immediately:

```text
harness.fs.snapshot({
  session_id: "sess_abc",
  scope_id: "tc_42",
  paths: ["/work/src/lib.rs"],
})
```

**Auto-on-write.** Omit `paths` and the snapshot is "open" — the
mutating tool builtins (`harness.tools.write_file`,
`harness.tools.delete_file`) lazy-capture each path's pre-image into
the active open snapshot bound to the current tool-call dispatch context. The
agent loop binds that context while it dispatches a tool, so a single
`harness.fs.snapshot({session_id, scope_id})` registration is enough to roll
the next mutation back. This internal context is not a script-facing authority
grant.

## Storage layout

```text
<workspace>/.harn/state/snapshots/<session_id>/<snapshot_id>/
  manifest.json     # path -> { kind: "file" | "absent", body_hash?, mode? }
  bodies/<sha256>   # content-addressed; deduped across snapshots
```

Snapshot manifests record the snapshot id, originating scope id,
capture timestamp, workspace root, and per-path entries. File bodies
are content-addressed by SHA-256.

When a session bundle exceeds the per-session byte cap
(`harn_hostlib::fs_snapshot::DEFAULT_SESSION_BYTE_CAP`, 1 GiB) the
oldest snapshots are evicted in insertion order. Embedders can override
the cap per session with
`harn_hostlib::fs_snapshot::configure_session_byte_cap`. The ACP server
also calls `harn_hostlib::fs_snapshot::drop_session_snapshots` on
`session/close` so closed sessions never leak their snapshot bundles.

Snapshots are session-scoped and ephemeral. Durable rollback across
process restarts is out of scope; consumers that need it bundle the
relevant state into a session via `session/load`.

## ACP surface

The ACP adapter advertises the primitive in the `initialize` response
under `agentCapabilities.sessionCapabilities`:

```jsonc
{
  "sessionCapabilities": {
    "close": {},
    "list": {},
    "resume": {},
    "rollback": {},
    "redo": {},
    "restoreToolCall": {}
  }
}
```

Clients drive a restore with:

```jsonc
{
  "method": "session/restore_tool_call",
  "params": {
    "sessionId": "sess_abc",
    "toolCallId": "tc_42"
  }
}
```

The server routes the call through `harn_hostlib::fs_snapshot::restore`
and emits a canonical `session/update` carrying the restored paths and
a `_meta.harn.kind = "tool_call_restored"` discriminator:

Completed prompt-turn rollback uses the same snapshots through
`session/rollback` and `session/redo`. Harn owns the transcript checkpoint
stack; hostlib owns the file pre-images. During rollback the ACP adapter
captures redo file snapshots, restores the turn's original snapshots, and then
moves the transcript back to the checkpoint boundary.

```jsonc
{
  "sessionId": "sess_abc",
  "update": {
    "sessionUpdate": "tool_call_update",
    "toolCallId": "tc_42",
    "status": "restored",
    "_meta": {
      "harn": {
        "kind": "tool_call_restored",
        "restoredPaths": ["/work/src/lib.rs"]
      }
    }
  }
}
```
