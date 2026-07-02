- **MCP client robustness: no more unbounded hangs.** All MCP OAuth HTTP
  requests (token exchange, refresh, discovery, dynamic registration) now use
  a client with a 30s request timeout and a 10s connect timeout — a token
  endpoint that accepts TCP but never responds can no longer wedge a refresh
  (and the single-flight refresh lock behind it) forever. The refresh lock
  itself is also bounded: the in-process mutex wait times out with a clear
  error naming the stuck holder, and the cross-process file lock uses a
  non-blocking try-lock with backoff instead of pinning a blocking thread
  indefinitely. On the stdio transport, response lines are capped at 64 MiB
  (protocol error instead of unbounded memory growth), and request writes now
  drain server output concurrently so a large request racing a flood of
  server notifications can no longer deadlock on full pipe buffers.
