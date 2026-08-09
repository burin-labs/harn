# ADR 0005: keep Harn's A2A adapter; do not adopt `a2a-lf`

## Status

Accepted on 2026-08-06 for
[#6089](https://github.com/burin-labs/harn/issues/6089). Harn keeps the
bespoke A2A server and client under `crates/harn-serve/src/adapters/a2a/` and
`crates/harn-vm/src/a2a/`. The pinned schema at
`conformance/protocols/schemas/a2a-0.3.0.schema.json` remains the contract of
record.

## Context

[#6072](https://github.com/burin-labs/harn/issues/6072) cut MCP over to the
official `rmcp` SDK. The same question was open for A2A. Harn currently carries
about 4,900 non-test lines of A2A adapter and client code (about 8,700 with
tests).

The official Rust SDK lives at
[a2aproject/a2a-rs](https://github.com/a2aproject/a2a-rs) and publishes under
`-lf` crate names. Verified on crates.io on 2026-08-06:

| Crate | Version | Role |
| --- | --- | --- |
| `a2a-lf` | 0.3.0 | Core types (`a2a::VERSION = "1.0"`) |
| `a2a-server-lf` | 0.4.1 | Async server framework |
| `a2a-client-lf` | 0.2.1 | Async client |

The crates.io name `a2a-rs` is a different, community crate
([emillindfors/a2a-rs](https://github.com/emillindfors/a2a-rs)). Any future
adoption must pin the `-lf` package names and say why in the manifest.

Harn's public A2A surface is A2A protocol 0.3.0, matching the upstream JSON
schema pinned with provenance to
`https://a2a-protocol.org/v0.3.0/specification`.

## Decision

Do not cut the A2A adapter over to `a2a-lf`, `a2a-server-lf`, or
`a2a-client-lf` on current evidence.

Do not adopt `a2a-lf` for types only. A types-only import still fails the
falsifier below: SDK types cannot round-trip Harn's pinned 0.3.0 fixtures
without field renaming or a Harn-owned shim.

Keep ownership as it is today:

- Harn owns transport, auth, task lifecycle, streaming, push notifications,
  workflow extensions, and audit seams.
- The pinned 0.3.0 schema and conformance fixtures under
  `conformance/protocols/` own the wire contract.

Revisit when either of these is true:

1. `a2a-lf` (or a successor under the same official project) exposes A2A 0.3.0
   wire shapes that deserialize Harn's pinned fixtures without a shim, or
2. Harn deliberately moves its public A2A pin to the SDK's protocol generation
   and regenerates conformance against that pin.

Until then, do not add the `-lf` crates as workspace dependencies.

## Falsifier

Cutover, including types-only adoption, is justified only if a conformance run
against the pinned upstream A2A 0.3.0 schema passes using unmodified SDK types
with no Harn-side field renaming or shim. If SDK types need a fork or adapter
layer to speak Harn's wire, ADR 0003's earlier MCP lesson applies and the
bespoke adapter stays.

## Evidence

A throwaway probe depended on `a2a = { package = "a2a-lf", version = "0.3.0" }`
and tried to deserialize Harn's committed A2A fixtures. Observed on
2026-08-06:

| Probe | Result |
| --- | --- |
| `a2a::VERSION` | `"1.0"`, not Harn's pin `"0.3.0"` |
| SDK `Message` serialize | `role: "ROLE_USER"`, field `messageId` |
| Harn fixture `agent_card.valid.json` → SDK `AgentCard` | missing field `supportedInterfaces` |
| Harn fixture `task_and_stream.valid.json` message → SDK `Message` | `Part must have one of: text, raw, url, data` (0.3.0 file parts use `{ "type": "file", "file": ... }`) |
| Harn status `"working"` → SDK `TaskState` | unknown variant; SDK expects `TASK_STATE_WORKING` |

Material shape gaps between Harn's 0.3.0 pin and `a2a-lf` 0.3.0:

- Protocol generation: `0.3.0` vs SDK `1.0`.
- Roles: `user` / `agent` vs `ROLE_USER` / `ROLE_AGENT`.
- Task states: `working` vs `TASK_STATE_WORKING`.
- Message identity: `id` vs `messageId`.
- Parts: typed 0.3.0 parts (`type` + `file` / `data`) vs SDK field-presence
  parts (`text` / `raw` / `url` / `data`).
- Agent card: `preferredTransport` + `additionalInterfaces[].transport` vs
  SDK `supportedInterfaces[].protocolBinding`.

Server and client crates were not evaluated further. Types already fail the
falsifier, and those crates remain pre-1.0 with lower adoption than the MCP
SDK path that justified ADR 0003.

## Consequences

- No dependency on `a2a-lf`, `a2a-server-lf`, or `a2a-client-lf`.
- Continue refreshing the pinned schema from the upstream 0.3.0 source named in
  `x-harn-provenance` when the contract changes.
- Treat crates.io name `a2a-rs` as a supply-chain hazard in reviews: the
  official package names end in `-lf`.
- The companion ACP evaluation
  ([#6088](https://github.com/burin-labs/harn/issues/6088)) stays independent;
  this ADR does not decide ACP ownership.

See [MCP, ACP, and A2A integration](../mcp-and-acp.md) for the public A2A
server contract.
