# Migrating pure collection method names

Harn collection methods return new values and never modify their receivers.
Their names now make that value semantics explicit:

| Legacy spelling | Canonical replacement |
|---|---|
| `list.push(value)` | `list.appending(value)` |
| `list.pop()` | `list.dropping_last()` |
| `list.sort()` | `list.sorted()` |
| `list.sort_by(key)` | `list.sorted_by(key)` |
| `list.reverse()` | `list.reversed()` |
| `string.reverse()` | `string.reversed()` |
| `set.add(value)` | `set.adding(value)` |
| `set.remove(value)` / `set.delete(value)` | `set.removing(value)` |
| `dict.merge(other)` | `dict.merging(other)` |
| `dict.remove(key)` | `dict.removing(key)` |
| `dict.rekey(fn)` | `dict.rekeyed(fn)` |

Only the names changed. Return values, ordering, copy-on-write behavior, and
error behavior are unchanged. In particular, `dropping_last()` returns a list
without its last item; use `last()` to read the last item.

The legacy spellings remain behavior-compatible aliases so existing scripts
can upgrade without a flag day. New code should use the canonical spellings.
Replace legacy collection method calls, then run:

```sh
harn check .
harn lint .
harn fmt --check .
```

Apply replacements to collection receivers, not unrelated APIs or user methods.
Calls such as `git.push(...)`, `stream.merge(...)`, storage `delete(...)`, and a
user-defined `add(...)` retain their existing names.
