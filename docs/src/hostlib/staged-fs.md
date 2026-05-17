# Staged filesystem (hostlib)

`harn-hostlib` includes a session-scoped filesystem staging layer for
hosts that want agents to accumulate a diff before applying it to the
working tree.

The capability is registered by `harn_hostlib::install_default` as the
`fs` module:

| Method | Result |
|--------|--------|
| `hostlib_fs_set_mode` | Switches a session between `immediate` and `staged`, returning the previous mode. |
| `hostlib_fs_staged_status` | Lists pending writes/deletes with byte counts and oldest pending age. |
| `hostlib_fs_commit_staged` | Applies all pending changes, or only selected paths, to disk. |
| `hostlib_fs_discard_staged` | Drops all pending changes, or only selected paths. |

Staged data is stored below:

```text
<workspace>/.harn/state/staged/<session_id>/
```

The manifest records the active mode, root, and pending entries. File
bodies are stored by SHA-256 content hash under `bodies/`, and
`journal.jsonl` records each staging operation for audit/debugging.

When a session is in `staged` mode, hostlib mutating tools write into
the overlay:

- `hostlib_tools_write_file` records a staged write.
- `hostlib_tools_delete_file` records a staged delete.

Read helpers use the same overlay before falling back to disk:

- `hostlib_tools_read_file`
- `hostlib_tools_list_directory`
- `hostlib_tools_get_file_outline`
- `hostlib_ast_parse_file`
- code-index file read/hash helpers

The ACP adapter exposes host controls for the same state:

- `session/fs_mode` with `{ sessionId, mode }`
- `session/fs_commit_staged` with `{ sessionId, paths? }`

Every staging mutation emits a `session/update` progress extension with
`_meta.harn.kind = "staged_writes_pending"`, `_meta.harn.pendingCount`,
and `_meta.harn.totalBytes`.
