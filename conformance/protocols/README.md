# Protocol conformance fixtures

`harn test protocols` validates checked-in ACP, A2A, and MCP JSON wire fixtures
against pinned JSON Schema snapshots. This gate is intentionally separate from
`harn test conformance`, which remains the executable Harn language/runtime
suite.

The schemas are compact Harn adapter profiles derived from the public protocol
schemas/specifications cited in each schema file. They pin the protocol surface
Harn currently emits or accepts and include negative fixtures for unsupported
versions, missing required fields, and unsupported extension discriminators.

Run:

```sh
make protocol-conformance
```

or:

```sh
cargo run --bin harn -- test protocols
```
