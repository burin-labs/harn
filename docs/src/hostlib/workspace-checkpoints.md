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
method returns. `session/close` drops the session's checkpoint state.

Set `workspaceCheckpoint: true` on `session/new` or a cold `session/load` to
capture a boundary when a prompt starts a turn. The prompt must carry a
nonempty `messageId`; Harn uses that identity as the boundary ID. The default
is `false`, and a server without
hostlib rejects `true`. A fork inherits this setting. A repeated `messageId`
is rejected in an opted-in session. The session response confirms the
setting in `workspaceCheckpointTracking`; clients must not treat a missing
field as enabled. Captures can be unavailable without blocking the prompt.
Query `plan` before offering file restoration; a failed
capture returns `{available: false, reason: "capture_failed"}` from `plan`
and `restore`, even if an older boundary used that ID. Loading an already-live
session cannot change its tracking setting. A cold load starts a new in-memory
checkpoint history; it doesn't restore earlier file boundaries. A steer
inserted into a running turn captures its own boundary immediately before its
message enters the transcript. If a tool call is still in flight or capture
otherwise fails, `plan` and `restore` report `capture_failed` for that message.
The host must bracket tool and shell calls with `call_begin` and `call_end` and
capture any additional boundaries it needs.
After a session is reanchored, checkpoint actions other than `drop` return
`{available: false, reason: "workspace_root_changed"}` while their recorded
root differs from the session's current root.

An ACP client can pass `inheritWorkspaceCheckpoint: true` to `session/fork`.
The fork response then includes `workspaceCheckpoint`, which reports whether
the parent's checkpoint history was copied to the new session. An unavailable
result is explicit and does not prevent the conversation fork. The copy has
independent checkpoint state and stored files, so restoring it or closing the
parent does not change the other branch's history. Forks without this option
do not copy checkpoints. Checkpoint state is still in memory and does not
survive a server restart.

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

On Unix, restore opens the workspace root once and resolves every path with
no-follow `openat` calls, creates missing directories with `mkdirat`, writes
through an exclusive temp file and `renameat`, and deletes with `unlinkat`. The
guard hash is read again through the same directory descriptor just before the
rename. A directory swapped for a symlink during a restore cannot redirect a
write outside the root; a path whose walk meets a symlink or non-directory is
reported in `failed`. Other targets resolve paths by name and check ancestors
first, which leaves a window for a swap between the check and the write.

A full restore with nothing refused, failed, or unrestorable discards later
boundaries and their attributions. Otherwise only the restored paths leave the
attribution, so a retry keeps its guard.

If the initial scan cannot read the workspace, capture returns
`incomplete_capture` and records no boundary. If a scan fails during a later
call, planning and restoring that boundary return `incomplete_attribution`.
An empty list of observed changes cannot prove that an unreadable directory
contained no changes. Planning, restoring, and copying checkpoint history
return `calls_in_flight` while a checkpointed call has no end receipt.

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
- Declared Harn runtime state is not a workspace edit. This includes the
  session database and sidecars, writer locks, checkpoints, and scanner state,
  including source-relative state in nested directories. Other `.harn` files
  remain eligible for attribution and restore.
- Clean tracked content depends on the git objects for the boundary's index
  staying in the object store. Garbage collection can remove them after the
  index moves on.
- State is in memory. Stored files live with the session and are removed by
  `checkpoint_drop`.
