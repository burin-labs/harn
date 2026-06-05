# Per-tool-call filesystem snapshots (hostlib)

`harn-hostlib` ships a Gemini-style `/restore` primitive paralleling the
[staged filesystem mode](staged-fs.md): a snapshot captures the
pre-image of paths a single mutating tool call is about to touch, so a
client can surgically roll the change back without affecting untracked
work.

The capability is registered by `harn_hostlib::install_default` as part
of the `fs` module:

| Method | Result |
|--------|--------|
| `hostlib_fs_snapshot` | Register (and optionally pre-capture paths for) a snapshot keyed by `scope_id` — canonically the ACP `toolCallId`. |
| `hostlib_fs_restore` | Write the captured pre-image back onto disk; delete paths the snapshot saw as absent. |
| `hostlib_fs_list_snapshots` | List the snapshots registered for a session, sorted by capture time. |
| `hostlib_fs_drop_snapshot` | Remove a snapshot's in-memory and on-disk state. |

## Capture modes

**Explicit.** Pass a `paths` list to `hostlib_fs_snapshot`; the bytes
are copied into the snapshot immediately:

```text
hostlib_fs_snapshot({
  session_id: "sess_abc",
  scope_id: "tc_42",
  paths: ["/work/src/lib.rs"],
})
```

**Auto-on-write.** Omit `paths` and the snapshot is "open" — the
mutating tool builtins (`hostlib_tools_write_file`,
`hostlib_tools_delete_file`) lazy-capture each path's pre-image into
the active open snapshot bound to the current
`harn_vm::agent_sessions::current_tool_call_id`. The agent loop sets
that thread-local automatically when it dispatches a tool call, so a
single `hostlib_fs_snapshot({session_id, scope_id})` registration is
enough to roll the next mutation back.

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
