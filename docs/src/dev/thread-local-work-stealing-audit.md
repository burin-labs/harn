# Thread-Local Work-Stealing Audit

Harn still has VM runtime state stored in `thread_local!` slots. That was
acceptable while VM work was pinned to one `LocalSet`, but pool workers now
cross the first work-stealing boundary: `std/lifecycle/pool` runs submitted
closures on `tokio::spawn` so they can execute on Tokio's multi-thread runtime.

The authoritative inventory is
`crates/harn-vm/thread_local_audit.toml`. It classifies every
`thread_local!` macro site under `crates/harn-vm/src` into one of three
categories:

| Category | Meaning | Required action |
| --- | --- | --- |
| `logical_task` | Execution context that must follow a logical VM task. | Convert to `tokio::task_local!` scope or explicit `Vm` / `AsyncBuiltinCtx` fields before relying on it from work-stealing tasks. |
| `runtime_registry` | Runtime/resource registry that a VM or host owns. | Promote to an `Arc`-backed registry and scope or pass that handle into spawned work. |
| `thread_private` | Cache, mock, warning de-dupe, or test harness state. | Keep thread-local only while it stays non-authoritative and resettable. |

`crates/harn-vm/tests/thread_local_audit.rs` scans the VM source tree and
fails when a new `thread_local!` site is added without an audit entry. The
same test also protects the pool worker boundary: pool registry state must
stay `tokio::task_local!`, not regress to a process-thread-local registry.

When adding a new pool, trigger, agent, or supervisor path that uses
`tokio::spawn`, audit the reachable builtins against this inventory first.
Do not copy a `thread_local!` value into a spawned task as a shortcut; convert
the owning state to a task-local or an explicit handle so it follows the
logical VM task rather than the OS worker thread.
