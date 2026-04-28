# Migration: harn-hostlib host contracts

`harn-hostlib` began as a migration path for code-intelligence and tool
surfaces that had lived in `burin-labs/burin-code`, including the Swift
`BurinCore`, `Sources/ASTEngine`, and `Sources/BurinCodeIndex` modules.
That history explains the early parity tests and the initial schema names,
but it is no longer the ownership model.

The current contract is Harn-owned:

- JSON schemas under `crates/harn-hostlib/schemas/` define request and
  response compatibility for every hostlib method.
- `HostlibRegistry` is the authoritative runtime catalog of registered
  modules and methods.
- Consumer repositories should treat their bridge tests as compatibility
  checks against Harn's published contract, not as the source of truth for
  hostlib behavior.

During migration, keep any consumer-specific bridge notes in this page or
in the consumer repository. Public hostlib docs, schema descriptions, and
module comments should describe the neutral Harn contract first.
