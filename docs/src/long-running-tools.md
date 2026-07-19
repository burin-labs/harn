# Long-running tools

Long-running tool handles let a script start slow work, continue the agent
loop, and receive the final result through the pending feedback queue on a
later turn. The idiom is the same for host command tools and stdlib operations
that support `long_running: true` or `background: true`.

Supported stdlib operations:

- `walk_dir(path, {long_running: true, ...})`
- `glob(pattern, base?, {long_running: true})`
- `glob(pattern, {base: "...", long_running: true})`
- `find_text(root, pattern, {long_running: true, ...})`
- `find_evidence(roots, patterns, {long_running: true, ...})`

Supported host tools:

- `tools.run_command`
- `tools.run_test`
- `tools.run_build_command`

`std/command` is also the script-facing command lifecycle. `command_run` passes
background request fields through to the host and returns the handle and planned
output artifacts. `command_wait` joins completion,
`command_wait_for_output` parks until output matches, the process exits, or a
deadline expires, and `command_cancel` tears down the owned process group. The
output wait subscribes to capture publication; it does not poll artifact files.

```harn,ignore
import { command_cancel, command_run, command_wait_for_output } from "std/command"

const service = command_run(["my-service"], {background: true})
defer { command_cancel(service, {wait_result_ms: 5000}) }
const ready = command_wait_for_output(service, "listening", {
  source: "stderr",
  timeout_ms: 10000,
})
if !ready.matched { throw "service did not become ready: " + ready.status }
```

## Process lifecycle

Foreground command tools (`run_command` without `background`, plus
`run_test`, `run_build_command`, `manage_packages`, and the VM-side
`process.exec` / `shell` / `exec_opts` builtins) tie their subprocess to
the invoking scope. The child runs in its own process group; when the
invoking scope is cancelled, a `deadline` expires, or the VM is dropped,
the whole group — grandchildren included — receives SIGTERM and, after a
2-second grace period, SIGKILL. The tool response reports
`status: "killed"`, and the pipeline observes the usual cancellation or
`Deadline exceeded` error. Group semantics are Unix-first; on Windows the
runtime kills the direct child (best effort) instead.

Work that must outlive the invoking scope belongs in a background handle:
`background: true` (or legacy `long_running: true`) children are exempt
from scope cancellation and deadlines. They are owned by the handle store
and die only through `tools.cancel_handle` or agent-session-end cleanup,
as described below.

## Handle envelope

A long-running call returns immediately with a handle envelope:

```harn
const handle = walk_dir(".", {long_running: true})
```

The returned dict includes:

```text
{
  handle_id: string,
  started_at: string,
  status: "running",
  command_or_op_descriptor: string
}
```

Command tools also include command-specific fields such as `command_id`, `pid`,
planned output paths, and sandbox metadata.

## Lifecycle

1. Spawn the operation with `long_running: true` or `background: true`.
2. Save `handle_id`.
3. Let the agent loop poll normally. Background workers push a `tool_result`
   item to the pending feedback queue when they complete.
4. Cancel abandoned work with `tools.cancel_handle`.
5. Rely on session-end cleanup only as a backstop. When an agent-loop session
   ends, registered resource managers cancel remaining handles for that
   session.

## Correct cleanup

Use `defer` or `finally` so early returns and thrown errors still release the
handle when the script no longer needs the result.

```harn
pipeline main() {
  const handle = walk_dir(".", {long_running: true})
  defer {
    host_tool_call("cancel_handle", {handle_id: handle.handle_id})
  }

  agent_loop("Summarize the repository while the file walk runs.", nil, {
    tools: ["read_file"],
  })
}
```

When the operation finishes before the cleanup path runs, cancellation returns
`cancelled: false`; that is expected because the handle has already left the
in-flight store and its result has been queued.

## Incorrect lifecycle

This starts background work but has no cleanup path if the pipeline exits early:

```harn
pipeline main() {
  const handle = walk_dir(".", {long_running: true})
  log(handle.handle_id)
}
```

`harn lint` warns for this shape with `long-running-without-cleanup`. Add a
`defer` or `finally` block that calls `tools.cancel_handle`.

## Feedback shape

Completed stdlib operations enqueue a `tool_result` payload like:

```json
{
  "handle_id": "hso-...",
  "status": "completed",
  "operation": "walk_dir",
  "command_or_op_descriptor": "walk_dir /repo",
  "started_at": "2026-04-30T12:00:00Z",
  "ended_at": "2026-04-30T12:00:01Z",
  "duration_ms": 1000,
  "result": []
}
```

Failed operations use `status: "failed"` and include `error`.
