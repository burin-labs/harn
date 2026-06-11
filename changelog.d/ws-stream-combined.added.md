- **One `harn serve site` route can now be both a WebSocket upgrade and an
  SSE/stream route.** A routed `pub fn` may carry `@ws` *and* `@stream`
  together: after the route runs auth + `@scopes` admission once, the site
  adapter sniffs the request head's `Upgrade: websocket` / `Connection:
  upgrade` headers (before any extractor or the body is consumed) — a genuine
  handshake takes the `SiteStreamProvider::upgrade` path, while every other
  request falls through to the `SiteStreamProvider::open` (SSE/stream) path
  instead of being refused with a 4xx. This is the seam the harn-cloud gateway
  `/acp` carve-out needs (one route, two transports, one admission). The
  previous `@ws`∧`@stream` conflict diagnostic (`HARN-SRV-016`) now fires only
  for `@ws`∧`@raw` (a handshake carries no request body, but `@raw` exists to
  buffer one). `@ws`-only and `@stream`-only routes are unchanged: a non-upgrade
  request to a `@ws`-only route is still refused with the correct 4xx by axum's
  extractor, and the `SiteStreamProvider::upgrade` default impl still returns
  `426 Upgrade Required` so embedders that implement only `open` keep compiling.
