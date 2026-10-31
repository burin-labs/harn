An ACP server embedder can answer `host/capabilities` and `host/call` itself
with `AcpServerConfig::with_host_responder`. A session's host requests then
never reach the connected client, so a standard editor that is not a host can
run a full turn over stdio or WebSocket. The agents API server uses the same
responder through the ACP bridge instead of a separate interception path.
