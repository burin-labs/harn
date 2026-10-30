# Run-tape branch contract

The `harn_vm::run_tape` host API records production model calls, tool dispatches,
prompt sources, and observed workspace changes. A branch serves a recorded prefix
and continues with live inference after the selected model call.

## Recording

`start_recording` accepts a recording level and an optional `TapeWorkspace`.
Only a full recording with a workspace effect observer can be branched.
Metadata recordings can be inspected but cannot reproduce a prefix. A process
can hold one active recording.

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

During the prefix, supported foreground commands return their recorded results
and apply their recorded workspace changes. Their policy hooks and processes do
not run again. Changing a command's arguments makes the prefix diverge. A host
must read `BranchGuard::finish` and treat divergence or an unfinished prefix as
failure.

## Refusals and limits

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
