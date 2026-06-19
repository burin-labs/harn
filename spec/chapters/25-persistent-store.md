## Persistent store

Six builtins provide a persistent key-value store backed by the resolved Harn
state root (default `.harn/store.json`):

| Function | Description |
|---|---|
| `store_get(key)` | Retrieve value or nil |
| `store_set(key, value)` | Set key, auto-saves to disk |
| `store_delete(key)` | Remove key, auto-saves |
| `store_list()` | List all keys (sorted) |
| `store_save()` | Explicit flush to disk |
| `store_clear()` | Remove all keys, auto-saves |

The store file is created lazily on first mutation. In bridge mode, the
host can override these builtins via the bridge protocol. The state root can
be relocated with `HARN_STATE_DIR`.

