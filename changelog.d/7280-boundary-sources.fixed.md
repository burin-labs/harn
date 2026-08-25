`HARN-LNT-029` (untyped-dict-access) and `HARN-OWN-004`
(boundary-value-unvalidated) now report the same way whether source reads a
boundary result from the ambient global or from the typed
`harness.<capability>.<method>` that replaced it. Both previously stopped
reporting once a call site adopted the spelling `HARN-LNT-071` asks for.

The two rules also now read one shared list of boundary sources. They had kept
separate hand-maintained copies that disagreed on six names, so `mcp_call` was
linted but not type-checked, while `connector_call`, `host_tool_call`,
`http_download`, `http_stream_info`, and `llm_call_safe` were type-checked but
not linted. Each rule now covers the union, which can surface findings on code
that previously passed one gate but not the other.
