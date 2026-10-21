# Turn-boundary workspace checkpoints

A host rewinds a session to an earlier user message by forking the
conversation with `harness.agent.fork_at` (ACP `session/fork`) and, when the
person asks, restoring the files that the abandoned turns changed. Workspace
checkpoints are the file half of that rewind. Conversation and files are
independent: fork only, restore only, or both.

Checkpoint methods are part of the `fs` capability family. They need a git
worktree. A directory that is not one returns `{available: false, reason,
detail}` from every method, never an empty success.

## Methods

| Method | Result |
|--------|--------|
| `harness.fs.checkpoint_capture(request)` | Record a boundary. Request: `{session_id, boundary_id, root?}`. |
| `harness.fs.checkpoint_call_begin(request)` | Stat the tree before a tool or shell call. Request: `{session_id, call_id}`. |
| `harness.fs.checkpoint_call_end(request)` | Stat again and attribute each changed path, with its post-call hash, to the call. Request: `{session_id, call_id, side_effect?}`. |
| `harness.fs.checkpoint_plan(request)` | The restore plan to a boundary, without touching files. Request: `{session_id, boundary_id}`. |
| `harness.fs.checkpoint_restore(request)` | Execute the plan and return the receipt. Request: `{session_id, boundary_id, paths?}`; `paths` must be non-empty when present, and omitting it restores every attributed path. |
| `harness.fs.checkpoint_drop(request)` | Release the session's checkpoint state and stored files. |

A host calls `checkpoint_capture` once per user message, brackets every tool
and shell call with `checkpoint_call_begin` and `checkpoint_call_end`, and
passes the tool annotation's side-effect class as `side_effect`.

## ACP

`session/workspace_checkpoint` carries the same actions for ACP clients. Params:
`{sessionId, action, boundaryId?, callId?, sideEffect?, paths?}` where `action`
is `capture`, `call_begin`, `call_end`, `plan`, `restore`, or `drop`. The root is
the session's cwd, and the result is the same typed object the `harness.fs`
method returns. `session/close` drops the session's checkpoint state. The
server does not bracket tool calls itself: the host that owns the turn calls
`capture` at each user message and `call_begin` / `call_end` around each call.

## What a boundary stores

- The stat index (path, size, mtime in nanoseconds, inode) of the non-ignored
  tree, held in memory as the attribution baseline.
- A git tree written from the index. Clean tracked files resolve from the
  repository's object store.
- Bytes only for files that differ from the index: modified and untracked
  files, content-addressed under `.harn/state/checkpoints/<session>/blobs/`.
  A file above 64 MiB or a dirty symlink is listed as unrestorable.

A scan of about 10,000 files takes roughly 45 ms warm.

## Restore plan and guard

Restoring to boundary N considers only paths attributed to the session at or
after N. For each path the plan compares three states: the boundary content,
the session's last attributed post-call hash, and the content on disk now.

| Disk now | Plan action |
|----------|-------------|
| Equals the boundary content | `skip`: already matches |
| Differs from the last attributed hash | `refuse`: changed outside the session |
| Matches the last attributed hash, boundary content exists | `restore`, or `recreate` when the file is missing |
| Matches the last attributed hash, absent at the boundary | `delete` |

`checkpoint_restore` checks the guard again immediately before each write. The
check and the write are separate steps with no lock, so a writer that lands in
between is overwritten.

A full restore with nothing refused, failed, or unrestorable discards later
boundaries and their attributions. Otherwise only the restored paths leave the
attribution, so a retry keeps its guard.

If a directory was unreadable during a call, or a file vanished mid-scan, that
call's attribution for the affected paths is marked `incomplete` and a restore
refuses them.

## Receipt

`checkpoint_restore` returns `{available, boundary_id, restored, refused,
skipped, failed, irreversible}`. A write that fails is listed under `failed` and
the rest of the restore still runs. `restored`, `refused`, and `skipped` are plan entries
`{path, action, reason?, expected_hash?, found_hash?}`. `irreversible` lists
`{call_id, side_effect}` for calls after the boundary whose annotation is
`network` or `desktop_control`; a file restore cannot undo them.

## Known limits

- A different writer editing a file during a call's window is attributed to
  that call. The guard still refuses later divergence from the call's
  post-call hash.
- A same-size edit inside one filesystem mtime tick is invisible to the stat
  diff.
- Symlinks, nested repositories, and tracked files that an ignore rule matches
  are outside the index.
- Clean tracked content depends on the git objects for the boundary's index
  staying in the object store. Garbage collection can remove them after the
  index moves on.
- State is in memory. Stored files live with the session and are removed by
  `checkpoint_drop`.
