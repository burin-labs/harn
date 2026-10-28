The signals inbox requires `harn.signals.inbox.v3` so older writers cannot
discard captured-output ownership, ordering, or durable dispatch state.
Incompatible schemas are refused without rewriting.

Migration: Upgrade all consumers together and select a new inbox directory
with `HARN_EXT_SIGNALS_DIR` or `harn signals --dir`. Before: consumers share a
v1/v2 inbox. After: upgraded consumers share a new v3 inbox. Preserve the old
directory for audit and finish its outstanding work through its matching
version; no automatic conversion or deletion is performed.
