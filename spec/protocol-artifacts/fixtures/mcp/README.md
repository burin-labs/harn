# MCP 2026-07-28 wire fixtures

Hand-authored JSON request/response fixtures used by Harn's MCP stable
compatibility harness. The same fixtures are published here so downstream
host and cloud test suites can replay the canonical flows in their
own tests instead of copy-pasting wire shapes.

Each fixture file has the shape:

```json
{
  "name": "harn.mcp.<scenario>",
  "description": "Human-readable summary of the scenario.",
  "kind": "exchange | http_header_exchange | schema",
  "documents": [ ... ]
}
```

`documents` is interpreted by the loading test: request → response
sequences for `exchange`, header set → body pairs for
`http_header_exchange`, a single schema object for `schema`.

Sources:

- [MCP 2026-07-28 changelog](https://modelcontextprotocol.io/specification/2026-07-28/changelog)
- [Lifecycle](https://modelcontextprotocol.io/specification/2026-07-28/basic/lifecycle)
- [Transports](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports)
- [JSON Schema 2020-12](https://json-schema.org/draft/2020-12)
