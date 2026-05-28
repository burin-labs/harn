- **A.12 follow-on: per-route compression opt-out + push-hint builder
  (#2515).** Two more knobs land on top of the transport stack from
  #2571. Handlers can now mark an individual response as
  uncompressible by setting `x-compress: never` — useful for SSE
  routes where chunked compression breaks flushing semantics, or for
  already-compressed binary downloads where re-encoding wastes CPU.
  A new outer middleware strips the marker before the response leaves
  the server so clients never see the implementation detail. The
  default `tower-http::DefaultPredicate` is still consulted, so SSE /
  gRPC / image filtering continues to work for routes that don't
  opt out. The corresponding constants (`COMPRESSION_OPT_OUT_HEADER`,
  `COMPRESSION_OPT_OUT_VALUE`, `HeaderOptOutPredicate`) are re-exported
  from `harn-serve` for adapters that build their own predicate
  pipelines. `.harn` handlers gain `http_push_hints(envelope, paths)`
  for emitting HTTP/2 server-push hints via `Link: <path>; rel=preload;
  as=...` headers, with `as=` inferred from the asset extension
  (`.css` → `style`, `.js`/`.mjs` → `script`, image/font/json
  extensions handled, unknown extensions emit a bare `rel=preload`).
  As a drive-by, `http_codec::merge_headers` now correctly preserves
  every value of a multi-valued envelope header (Link, Set-Cookie)
  instead of silently dropping continuation values.
