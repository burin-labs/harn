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

Hosts using `harness.fs.checkpoint_capture` can require writer evidence by
setting `writer_policy: "require_known"` on the first capture. They must verify
that the receipt echoes that policy.
The policy stays with the session and its forks. A scan detects changed paths
but cannot identify their writers, so strict restore refuses those paths. The
default `legacy_scan` policy preserves scan-based behavior for existing hosts.
Plan entries report the writers and the observed file state. After obtaining a
person's confirmation, a host can pass `confirmed_paths` to restore as a list
of `{session_id, boundary_id, path, observed}` records copied from the plan.
A different session, boundary, or later file change does not match that
confirmation and is refused. The confirmation is supplied by the host; Harn
cannot verify that the host actually showed an approval prompt.
Strict restore currently recreates absent files only, using an atomic
no-replace operation on Unix. It refuses replacement and deletion of existing
files, even after confirmation, because another writer could change the path
between a content check and the mutation. Strict recreation is unavailable on
other platforms.

## ACP

`session/workspace_checkpoint` carries the same actions for ACP clients. Params:
`{sessionId, action, boundaryId?, callId?, sideEffect?, paths?}` where `action`
is `capture`, `call_begin`, `call_end`, `plan`, `restore`, or `drop`. The root is
the session's cwd, and the result is the same typed object the `harness.fs`
method returns. `session/close` releases an idle session's checkpoint state from
memory and keeps its record on disk for a later `session/load`; only the `drop`
action deletes the record.

Set `workspaceCheckpoint: true` on `session/new` or a cold `session/load` to
capture a boundary when a prompt starts a turn. The prompt must carry a
nonempty `messageId`; Harn uses that identity as the boundary ID. The default
is `false`, and a server without
hostlib rejects `true`. A fork inherits this setting. A repeated `messageId`
is rejected in an opted-in session. The session response confirms the
setting in `workspaceCheckpointTracking`; clients must not treat a missing
field as enabled. Captures can be unavailable without blocking the prompt.
When a tool call cannot be tracked (`call_begin` is unavailable, for example
after a failed index write), the server drops the session's checkpoints and
runs the call untracked. Earlier boundaries then plan as `no_checkpoint`;
later prompts capture new ones. The one exception: when the checkpoint record
can be neither deleted nor moved aside, the call is refused, because a later
load could otherwise resume history that never saw it. Loads, captures and
call admissions wait while a session's record is being dropped, so none of
them can resume or replace the history being retired.
Query `plan` before offering file restoration; a failed
capture returns `{available: false, reason: "capture_failed"}` from `plan`
and `restore`, even if an older boundary used that ID. Loading an already-live
session cannot change its tracking setting. Loading an opted-in session that
this process does not hold resumes the checkpoint history an earlier process
recorded under the session's root, so a restarted host can still plan and
restore against earlier boundaries. A steer
inserted into a running turn captures its own boundary immediately before its
message enters the transcript. If a tool call is still in flight or capture
otherwise fails, `plan` and `restore` report `capture_failed` for that message.
In an opted-in session the agent loop brackets every admitted tool call with
`call_begin` and `call_end`, keyed by its tool call ID and recorded with the
tool's declared side-effect class, so a restore plan names the files each call
changed. Calls declared `read_only` and calls without an ID are not bracketed.
A host that executes work outside the agent loop must bracket it itself and
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
do not copy checkpoints.

Each session records its boundaries, attributed calls, revision, and typed
complete or incomplete state in one index beside its stored files. A tracked
tool call is admitted only after the incomplete state is durably written; its
attribution is recorded before the index becomes complete again. A host restart
between those writes makes `resume`, `plan`, and `restore` report
`checkpoint_interrupted` until the checkpoint is dropped. A failed begin write
refuses the tool before its handler runs. If the index write fails before
replacement, the prior complete index remains available. If replacement
occurred but namespace durability could not be confirmed, a cold resume treats
the visible incomplete index as interrupted. A failed final call or restore
write keeps the index incomplete and returns the effect receipt with
`persistence_error`, so a completed workspace mutation is not mistaken for a
failed one. Embedding hosts use
`workspace_checkpoint::resume(session_id, root)` to load a complete record and
`workspace_checkpoint::unload(session_id)` to release it from memory while
keeping its record. Both the file contents and the namespace replacement must
be confirmed durable before a tracked tool is allowed to run.

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
`{call_id, invocation, side_effect}` for calls after the boundary whose annotation is
`network` or `desktop_control`; a file restore cannot undo them. A provider can
reuse a `call_id` in a later turn, so `invocation`, the session revision the call
began at, is what tells two such effects apart.

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
- The index and stored files live under the session's root and are removed by
  `checkpoint_drop`. A call in flight when the process exits cannot be
  attributed; its incomplete index is refused on restart and must be dropped.
