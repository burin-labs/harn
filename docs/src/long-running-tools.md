# Long-running tools

Long-running tool handles let a script start slow work, continue the agent
loop, and receive the final result through the pending feedback queue on a
later turn. The idiom is the same for host command tools and stdlib operations
that support `long_running: true` or `background: true`.

Supported stdlib operations:

- `walk_dir(path, {long_running: true, ...})`
- `glob(pattern, base?, {long_running: true})`
- `glob(pattern, {base: "...", long_running: true})`

Supported host tools:

- `tools.run_command`
- `tools.run_test`
- `tools.run_build_command`

## Handle envelope

A long-running call returns immediately with a handle envelope:

```harn
let handle = walk_dir(".", {long_running: true})
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
  let handle = walk_dir(".", {long_running: true})
  defer {
    host_tool_call("cancel_handle", {handle_id: handle.handle_id})
  }

  agent_loop({
    system: "Summarize the repository while the file walk runs.",
    tools: ["read_file"]
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
  let handle = walk_dir(".", {long_running: true})
  println(handle.handle_id)
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
