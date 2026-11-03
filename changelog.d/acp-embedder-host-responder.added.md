An ACP server embedder can answer `host/capabilities` and `host/call` itself
with `AcpServerConfig::with_host_responder`. The server binds a responder to
each session's workspace root for every prompt turn, so a session is never
answered with another session's workspace, and its host requests never reach
the connected client. A standard editor that is not a host can therefore run a
full turn over stdio or WebSocket. The agents API server uses the same binder
through the ACP bridge instead of a separate interception path.
