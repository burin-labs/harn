The signals inbox uses `harn.signals.inbox.v2` so older writers refuse captured
occurrences instead of discarding their exact session ownership and order.
Other schema versions are refused without rewriting. Use a separate inbox
directory for the new format and preserve any unfinished old-format inbox.
