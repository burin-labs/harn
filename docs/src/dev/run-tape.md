# Run-tape branch contract

The `harn_vm::run_tape` host API records production model calls, tool dispatches,
prompt sources, and observed workspace changes. A branch serves a recorded prefix
and continues with live inference after the selected model call.

## Recording

`start_recording` accepts a recording level and an optional `TapeWorkspace`.
Only a full recording with a workspace effect observer can be branched.
Metadata recordings can be inspected but cannot reproduce a prefix. A process
can hold one active recording.

Hosts declare additional durable storage directories in `TapeWorkspace.state_roots`.
The recording retains those roots so a branch can isolate writes to them.

`taped_command_run` wraps a host command before its policy hooks and execution.
It records the returned value and observed workspace changes. Commands inside
an agent tool dispatch belong to that dispatch's recording and are not recorded
again. The host identifies background requests before entering the wrapper;
command policy cannot rewrite a request's background setting.

## Branch execution

`start_branch` takes the tape, the number of model calls to serve, the restored
workspace, and a fresh durable-state root. Enter its `BranchScope` around the
execution. The host must supply the workspace state from before the recording;
the tape does not reconstruct that initial state.

Set `live_after_calls` to zero to change the first model request. The tape must
contain a recorded model call. Reads and tool operations before that call still
replay and must be consumed before live inference starts. A zero-prefix branch
that never reaches inference finishes with an `under-run branch` divergence.

Set `BranchPlan.host_state_roots` to the host's durable roots for the new execution,
including a fresh per-run home. Both recorded and current host roots remap into
the branch state directory; undeclared external roots remain refused in the prefix.

During the prefix, supported foreground commands return their recorded results
and apply their recorded workspace changes. Their policy hooks and processes do
not run again. Changing a command's arguments makes the prefix diverge. A host
must read `BranchGuard::finish` and treat divergence or an unfinished prefix as
failure.

A served tool dispatch replays recorded transcript field changes. For a child
created and closed entirely within that dispatch, the tape retains its initial
transcript, changes, and closure. Replay validates the initial transcript's type,
version, session identity, and required lists before applying workspace effects.
It applies changes separately from live sessions, without rerunning the child or
its hooks. A missing target without
that lifecycle evidence still refuses. A newly created session that remains open,
or a dispatch that closes a previously existing session, is marked unservable
when recorded. A refused dispatch is not counted as served.
Tape version 5 carries these lifecycle records. Field-change records retain their
version 4 shape, so existing recordings remain readable by the new runtime.

Workspace-location reads (`fs.cwd` and `fs.source_dir`) are recorded during the
prefix. Filesystem builtins resolve absolute paths under the recorded workspace
into the restored workspace before applying normal permission checks. A mapped
path that escapes through a parent component or symlink is refused. This mapping
does not rewrite prompt text, host arguments, or command strings.

## Refusals and limits

- Live commands in a relocated workspace are refused: their embedded paths
  require a host filesystem namespace. Workspace path mapping is not a sandbox
  for arbitrary host tools or external processes.
  A baseline recorded in an isolated workspace can replay at that same path;
  the host restores its initial contents between runs.
- Background commands cannot be served because their process handles do not
  exist in the branch. A dispatch that starts one is marked unservable.
- Overlapping dispatches cannot be served because their workspace scans and
  transcript changes do not establish a reproducible order.
- A dispatch containing nested inference cannot be served.
- Redacted payloads, incomplete file-effect observations, and unconsumed prefix
  operations prevent an exact replay.
- Only observed workspace changes are reapplied. External effects from the
  recording are not repeated.
- Dropping a branch scope does not cancel a session. Hosts must use their
  session cancellation path and wait for its terminal result.

Outstanding replay cases are tracked in
[remaining prefix cases](https://github.com/burin-labs/harn/issues/10805).
