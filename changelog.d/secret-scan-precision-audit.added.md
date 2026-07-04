- Added a `precision` class (`"high"` for self-identifying token shapes,
  `"heuristic"` for keyword/context matches like `Bearer <b64>` or
  `password = "..."`) to every `secret_scan` finding and to the canonical
  secret catalog. Consumers can now hard-block only high-precision findings
  without hard-coding detector names.
- Added an optional `{audit: false}` second argument to `secret_scan` so a
  hot-path caller (e.g. a per-edit or per-command guard) can get catalog-backed
  findings without appending an `audit.secret_scan` event on every call. The
  one-argument form still audits, unchanged.
