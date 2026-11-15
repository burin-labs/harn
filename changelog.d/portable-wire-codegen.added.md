Compile tool contracts with `harn codegen ir`, validate frozen IR documents, and
run installed code-generation backends with deterministic, confined artifact
publication. The closed wire graph preserves nominal and recursive types,
signed 64-bit integers, bytes, decimals, sets, tuples, absent fields, and declared
application errors across generated clients and native handlers.

Bind verified ordinary MCP workers through `std/tools.native_tool_registry`.
Native handlers retain the original tool registry's governance and adapter
metadata, with request-specific cancellation and an exact concurrency limit.
For MCP `2026-07-28`, closing a direct HTTP response cancels that request while
preserving independently owned background tasks.

See [Migrate typed tool clients](https://harnlang.com/migrate-typed-tools.html)
for the corrected wire representations and client update checks.
