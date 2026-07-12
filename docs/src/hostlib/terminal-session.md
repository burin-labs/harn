# Typed terminal sessions

`harn-hostlib` can drive an interactive terminal through six schema-backed
host operations. The implementation is cross-platform and terminal-native:
`harn-terminal` owns the pseudo-terminal and VT parser, while the hostlib
adapter owns policy, secret custody, session limits, and VM values.

The Cargo feature is `terminal-session`. It is included by `full`, but the
runtime capability remains disabled until the pipeline opts in:

```harn,ignore
hostlib_enable("terminal:session")
```

Compiling the feature does not expose it as an agent tool or enable any agent
policy automatically.

## Safety boundary

Terminal commands use an argv vector and never invoke an implicit shell.
`start` removes inherited secret-bearing environment variables, rejects
explicit secret-like environment keys, and applies Harn's universal
catastrophic-command floor before allocating a child.

The PTY backend cannot yet translate Harn's restricted process policy into
the platform PTY spawn. It therefore fails closed under `worktree`,
`os_hardened`, and `wasi` with a structured `sandbox_unsupported` exception.
Use terminal sessions only in an explicitly unrestricted, trusted harness:

```text
harn run --no-sandbox path/to/harness.harn
```

Do not use this flag for untrusted scripts. Sandboxed PTY spawning is a
separate capability from this initial contract.

## Lifecycle

Start a process with an argv vector and terminal dimensions:

```harn,ignore
let started = hostlib_terminal_session_start({
  argv: ["./target/debug/my-tui", "--offline"],
  cwd: project_root,
  rows: 30,
  columns: 100,
  env: {APP_HOME: isolated_home}
})
```

The response contains a bounded manager-generated `session_id`. A host keeps
at most eight sessions. Every remaining child is terminated when its manager
is dropped.

Send literal text and typed keys as one atomically validated batch:

```harn,ignore
hostlib_terminal_session_send_keys({
  session_id: started.session_id,
  events: [
    {type: "text", text: "alpha beta"},
    {
      type: "key",
      key: {kind: "character", value: "w"},
      modifiers: ["ctrl"]
    },
    {type: "key", key: {kind: "named", name: "enter"}}
  ]
})
```

Named navigation keys, F1–F12, Unicode characters, and explicit Control,
Alt, Shift, and Super modifiers are represented structurally. Combinations
without a portable terminal encoding are rejected before any bytes are sent.

Resize both the native PTY and parser with
`hostlib_terminal_session_resize`. Wait for output to settle with
`hostlib_terminal_session_wait_idle`; it uses terminal revision notifications,
not a polling sleep. The initial idle boundary requires either observed output
or a completely exited child, preventing a false idle result before a TUI's
first paint. For input-driven transitions, capture the revision before sending
keys and pass it as `after_revision`; the idle wait then cannot be satisfied by
output that was already quiet before the input:

```harn,ignore
let before = hostlib_terminal_session_capture({session_id: started.session_id})
hostlib_terminal_session_send_keys({
  session_id: started.session_id,
  events: [{type: "key", key: {kind: "named", name: "enter"}}]
})
hostlib_terminal_session_wait_idle({
  session_id: started.session_id,
  after_revision: before.revision,
  quiet_ms: 50,
  timeout_ms: 10000
})
```

Always call `hostlib_terminal_session_end` in cleanup. `end` closes the PTY,
terminates the child when needed, waits for the final output drain, and returns
its typed exit state.

## Capture contract

`hostlib_terminal_session_capture` returns:

- fixed `schema_version: 1`;
- dimensions and one normalized string per visible row;
- cursor row, column, and visibility;
- alternate-screen state;
- monotonic revision and received-byte counters;
- parser/reader diagnostics;
- running, exited, or failed child status.

Detailed cell capture is opt-in and bounded. Pass a rectangle such as
`region: {row: 0, column: 0, rows: 5, columns: 20}` to receive coordinates,
text, foreground/background color, bold/italic/underline/inverse attributes,
and wide-character/continuation flags for those cells. Omitting `region`
keeps ordinary captures compact.
