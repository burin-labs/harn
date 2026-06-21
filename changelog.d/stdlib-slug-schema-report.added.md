- **Stdlib slug names, structured schema reports, and clearer `??` formatting.**
  `std/slug` now provides Harn-written random and deterministic memorable name
  helpers, `schema_report(...)` exposes non-throwing path-aware validation
  issues, `std/schema` wraps those reports ergonomically, and `harn fmt`
  parenthesizes mixed `??`/comparison or logical expressions so the parser's
  grouping is visible.
