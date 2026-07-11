- Fixed session-store `close()` so a correctly closed, fully signed session verifies cleanly: `verify()`
  now attests the `Receipt` event against the pre-receipt chain root (via `verify_receipt_root`) instead of
  reporting a spurious `BadSignature`.
- Made SQLite `close()` atomic: the receipt insert, its signature, and the `status='closed'` flip now commit
  in a single transaction, so a crash mid-close can no longer leave a receipt behind an `open` session.
- Fixed the in-memory backend `close()` to sign the receipt it appended by event id under one lock, instead
  of signing `last_mut()` after releasing the guard where a concurrent append could displace it.
