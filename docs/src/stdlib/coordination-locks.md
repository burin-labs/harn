# Coordination directory locks

`std/coordination` provides filesystem directory leases for exclusive work. A
lease has an owner, a token and an expiry. Acquisition and release use the same
owner record for both scoped APIs.

| Function | Result | Conflict behavior |
|---|---|---|
| `coord_with_dir_lock` | The value returned by `body(lock)` | Throws the existing lock-conflict message. |
| `coord_try_with_dir_lock` | `Result<unknown, CoordDirLockConflict>` | Returns `Err` with the measured conflict. |

Both functions accept `runtime`, `fs`, `clock`, `random`, `resource_path`,
`body`, optional `owner`, and optional `options`, in that order. They release
the acquired lease before returning the body value or propagating its error.
A successful nil body result is `Ok(nil)` in the attempt API.

`CoordDirLockConflict` contains `schema`, `reason`, `resource_path`, `lock_path`
and `message`. Its schema is `harn.coordination.dir_lock_conflict.v1`. Reasons
are `active`, `stale`, `unrecorded`, `malformed` or `absent`; `absent` can occur
when the conflicting directory disappears before its owner is inspected.
The message includes available owner and expiry information.

The attempt API makes one acquisition attempt through the existing lease and
stale-owner recovery path. It always returns contention as `Err`, even when
`options.throw_on_conflict` is true. It does not wait or retry the body.
Filesystem, sandbox, body and release errors propagate as failures. They are
not lock-conflict results and must not enter a contention retry loop.

Underlying `coord_acquire_dir_lock` treats an atomic `mkdir` collision as
contention. Other creation errors retain their structured filesystem failure,
including `not_a_directory` and `permission_denied`.
