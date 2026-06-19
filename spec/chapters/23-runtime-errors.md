## Runtime errors

| Error | Description |
|---|---|
| `undefinedVariable(name)` | Variable not found in any scope |
| `undefinedBuiltin(name)` | No registered builtin or user function with this name |
| `immutableAssignment(name)` | Attempted `=` on a `let` binding |
| `typeMismatch(expected, got)` | Type assertion failed |
| `returnValue(value?)` | Internal: used to implement `return` (not a user-facing error) |
| `retryExhausted` | All retry attempts failed |
| `thrownError(value)` | User-thrown error via `throw` |

Most `undefinedBuiltin` errors are now caught statically by the
cross-module typechecker (see [Static cross-module
resolution](#static-cross-module-resolution)) — `harn check` and
`harn run` refuse to start the VM when a file contains a call to a name
that is not a builtin, local declaration, struct constructor, callable
variable, or imported symbol. The runtime check remains as a backstop
for cases where imports could not be resolved at check time.

### Stack traces

Runtime errors include a full call stack trace showing the chain of
function calls that led to the error. The stack trace lists each frame
with its function name, source file, line number, and column:

```text
Error: division by zero
  at divide (script.harn:3:5)
  at compute (script.harn:8:18)
  at default (script.harn:12:10)
```

Stack traces are captured at the point of the error before unwinding, so
they accurately reflect the call chain at the time of failure.

### Diagnostic code appendix

Runtime and static diagnostics use stable `HARN-<category>-<number>` codes.
The generated diagnostics catalog is authoritative for the full registry; the
language spec records runtime namespaces whose behavior affects portable
programs.

#### Suspend/resume diagnostics (`HARN-SUS-*`)

| Code | Description |
|---|---|
| `HARN-SUS-001` | `suspend_agent` was called on a worker that is not running. |
| `HARN-SUS-002` | `ResumeConditions` validation failed. |
| `HARN-SUS-003` | `resume_agent` was called on a live worker that is not suspended. |
| `HARN-SUS-004` | A resume snapshot was missing, stale, unreadable, or version-incompatible. |
| `HARN-SUS-005` | `agent_await_resumption` was invoked outside `agent_loop` structural handling. |
| `HARN-SUS-006` | A concurrent resume changed the worker before resume completed. |
| `HARN-SUS-007` | `conditions.trigger` could not be registered with the trigger dispatcher. |
| `HARN-SUS-008` | A timeout fired or was configured with an unsupported `on_timeout` action. |
| `HARN-SUS-009` | Resume input failed `agent_loop` input validation. |
| `HARN-SUS-010` | A worker closed while suspended rejected a later resume. |

#### OAuth diagnostics (`HARN-OAU-*`)

| Code | Description |
|---|---|
| `HARN-OAU-001` | A persisted sink redacted an OAuth-shaped token (transcript / receipt / OTel / system reminder). The original value still flows through to the tool — redaction is display-only. |
| `HARN-OAU-002` | `std/oauth/client` cannot refresh because no `refresh_token` is in storage (or the server rejected the refresh). Re-run `start_authorization` / `device_flow`. |
| `HARN-OAU-005` | `std/oauth/dynamic_registration` rejected a candidate client metadata document under RFC 7591 §2. |

#### Durable agent channels diagnostics (`HARN-CHN-*`)

| Code | Description |
|---|---|
| `HARN-CHN-001` | `pipeline:` scope used outside any active pipeline context. |
| `HARN-CHN-002` | Cross-tenant `emit_channel` without a grant, or `org:` scope (disabled in v1 until org grants are available). |
| `HARN-CHN-003` | Malformed channel name, scope prefix, or scope id. |
| `HARN-CHN-004` | Scope ambiguous — explicit `options.session_id` or `options.pipeline_id` conflicts with the active runtime context. |
| `HARN-CHN-005` | Malformed `batch` config on a `trigger_register` call (missing `count`/`window`, non-positive `count`, unknown `expire_action`, or wrong-typed `key`). |

#### Agent pool diagnostics (`HARN-POL-*`)

| Code | Description |
|---|---|
| `HARN-POL-001` | A `backpressure_queue(..., "fail_submitter")` pool was full at submit time. Drop policies do not raise — they return a `rejected` task handle and emit a `pool_drop` audit on `lifecycle.pool.audit`. |
| `HARN-POL-002` | A `fail_fast()` pool had no immediate capacity at submit time. The submitter receives the error synchronously; no task is enqueued. |

