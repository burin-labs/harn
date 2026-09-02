# Agent Plugins packages

Harn can inspect and validate packages that implement the published
[Agent Plugins 1.0 specification](https://agent-plugins.org/specification).
One package can carry portable metadata, Agent Skills, and MCP server launch
configuration. Harn parses the package once and exposes a typed result to CLI,
runtime, and embedding hosts.

## Package layout

`plugin.json` is required at the package root. `skills/` and `mcp.json` are
optional components.

```text
acme-tools/
├── plugin.json
├── mcp.json
└── skills/
    └── deploy/
        └── SKILL.md
```

The minimum manifest is:

```json
{
  "$schema": "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json",
  "name": "acme.tools"
}
```

Validate the complete package with a host-selected persistent data directory:

```bash
harn plugin validate ./acme-tools \
  --data-dir "$HOME/.local/share/acme-tools"
```

Use `--json` to receive the stable load report. `accepted` says whether the
root manifest admitted the package. `conformant` says whether every normative
check passed. Invalid skills, an invalid MCP component, and invalid individual
MCP server entries are reported and isolated at their specification-defined
boundaries.

```bash
harn plugin inspect ./acme-tools --json
```

`inspect` exits successfully for an accepted package even when an optional
component has diagnostics. `validate` exits nonzero for any conformance error
or warning. Both commands print the same structured report.

## MCP configuration

Harn supports the required `stdio` and `streamable-http` variants. It reports
the optional legacy `sse` variant as unsupported and skips only that server.

```json
{
  "$schema": "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json",
  "mcpServers": {
    "local": {
      "type": "stdio",
      "command": "node",
      "args": ["${PLUGIN_ROOT}/server.js"],
      "env": { "STATE": "${PLUGIN_DATA}/state.json" },
      "cwd": "${PLUGIN_ROOT}"
    },
    "hosted": {
      "type": "streamable-http",
      "url": "https://tools.example.com/mcp",
      "headers": { "X-Tenant": "acme" }
    }
  }
}
```

For stdio servers, Harn expands only the exact `${PLUGIN_ROOT}` and
`${PLUGIN_DATA}` placeholders, once. It supplies both reserved environment
variables after applying configured environment values. Commands and working
directories cannot escape their declared roots through `..`, absolute paths,
or symlinks.

For remote servers, non-loopback URLs require HTTPS. URLs cannot contain
userinfo or fragments. Header names are compared without ASCII case so a
configuration cannot smuggle duplicate headers through different spelling.
Header values are literal and never perform environment or placeholder
expansion; do not put secrets in `mcp.json`.

## Client extensions

Agent Plugins 1.0 deliberately leaves commands, hooks, agents, rules, language
servers, and client-specific presentation outside its portable core. Put such
metadata under a reverse-domain key in `extensions` or a matching top-level
directory. Harn reserves `org.harnlang` for its typed extensions. A client that
does not implement an extension ignores its contents.

This separation is the intended bridge to richer generated surfaces: portable
skills and MCP launch data remain interoperable, while Harn-owned extension
schemas can describe command trees, OpenAPI projections, and other adapters
without inventing a second package format.
