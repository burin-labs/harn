# Protocol conformance fixtures

`harn test protocols` validates checked-in ACP, A2A, and MCP JSON wire fixtures
against pinned JSON Schema profiles. This gate is intentionally separate from
`harn test conformance`, which remains the executable Harn language/runtime
suite.

The schemas are Harn adapter profiles backed by the public protocol schemas and
specifications cited in each schema file's `x-harn-provenance` block. The
upstream MCP and A2A JSON schema artifacts are definitions-only, so each Harn
profile supplies the root over the request, response, notification, metadata,
and negative-case shapes Harn actually emits or accepts.

Every fixture is a matrix row:

```json
{
  "name": "mcp.initialize.2025_11_25",
  "protocol": "mcp",
  "schema": "schemas/mcp-2025-11-25.schema.json",
  "expect": "valid",
  "documents": [],
  "matrix": {
    "version": "2025-11-25",
    "family": "initialize",
    "case": "success",
    "source": {
      "kind": "official_example",
      "url": "https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle",
      "description": "Why this row exists."
    }
  }
}
```

`matrix.source.kind` is one of:

- `official_example`: derived from an official example or normative schema
  semantics; `url` is required.
- `adapter_generated`: generated from Harn adapter code; `generator` is
  required and should name the focused test that pins the fixture.
- `hand_authored`: maintained by hand against the pinned schema profile;
  `description` is required.

Run:

```sh
make protocol-conformance
```

or:

```sh
cargo run --bin harn -- test protocols
```

Useful filters:

```sh
cargo run --bin harn -- test protocols --filter mcp
cargo run --bin harn -- test protocols --filter 're:^(mcp|a2a)\\.agent_card'
cargo run --bin harn -- test protocols acp --verbose
```

## Refreshing profiles

When an upstream protocol version changes:

1. Run the `refresh_command` recorded in the relevant schema's
   `x-harn-provenance` block and inspect the upstream diff.
2. Update the Harn profile schema to match the adapter surface for the new
   version.
3. Update fixture `matrix.version` values and add success/negative rows for
   newly supported or intentionally unsupported protocol families.
4. Run `make protocol-conformance`.
5. Run the adapter drift tests for any `adapter_generated` fixtures you touched,
   for example:

```sh
cargo test -p harn-serve adapter_protocol_fixture_matches_checked_in_matrix
cargo test -p harn-serve adapter_agent_card_protocol_fixture_matches_checked_in_matrix
cargo test -p harn-serve protocol_conformance_
```

Adapter-generated fixture tests print the expected and actual JSON arrays when
they drift, so CI failures should point directly at the checked-in fixture that
needs to be refreshed or the adapter behavior that changed unexpectedly.
