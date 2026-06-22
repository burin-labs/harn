# ACP Agent Registry submission

This directory holds Harn's entry for the official
[ACP Agent Registry](https://agentclientprotocol.com/registry) — the
directory of ACP coding agents that Zed and JetBrains users install with one
click from inside the editor ([Zed announcement][zed], [JetBrains][jb]).
Tracks [#2672](https://github.com/burin-labs/harn/issues/2672).

The registry lives at
[`agentclientprotocol/registry`](https://github.com/agentclientprotocol/registry).
Each agent ships a `<id>/agent.json` manifest plus a 16×16 monochrome
`icon.svg`; CI validates every PR against
[`agent.schema.json`][schema] and an ACP auth handshake. Submission is a fork +
PR per [`CONTRIBUTING.md`][contrib]; once merged the agent is available in all
ACP-speaking clients.

## What's here

- [`harn/agent.json`](harn/agent.json) — the registry manifest entry, pinned to
  the v0.8.133 binary distribution (GitHub release tarballs for the five
  published targets), launching `harn serve acp`.
- [`harn/icon.svg`](harn/icon.svg) — 16×16 `currentColor` icon.

This is the agent's free editor discoverability listing. It is **not** the
signed skills/connectors marketplace (harn-cloud#24).

## Validation

The manifest is ready for an upstream registry PR. Run from a clone of
`agentclientprotocol/registry` with `harn/` copied in:

```bash
uv run --with jsonschema .github/workflows/build_registry.py
python3 .github/workflows/verify_agents.py --auth-check --agent harn
```

The checked-in Harn test suite also guards the local copy:

- `crates/harn-cli/src/cli/tests/parse_serve.rs` proves the registry launch
  form (`harn serve acp`, no positional file) parses as the file-less attach
  server.
- `crates/harn-serve/src/auth.rs` proves an empty local auth policy advertises
  a non-empty ACP `authMethods` array with `type: "agent"`.
- `crates/harn-cli/tests/acp_registry_manifest.rs` keeps this manifest pinned
  to the current Harn version, five published binary targets, and the
  `["serve", "acp"]` launch arguments.

These guards correspond to the registry's auth check. The upstream validator
launches the binary with the manifest `cmd` + `args` (`./harn serve acp`),
sends ACP `initialize`, and requires at least one auth method whose type
resolves to `agent` or `terminal`; Harn's no-credential local attach flow now
advertises the `none` method as `type: "agent"`.

## Submitting

1. Re-verify locally against a fresh clone of `agentclientprotocol/registry`:

   ```bash
   git clone https://github.com/agentclientprotocol/registry
   cp -R spec/acp-registry/harn registry/harn
   cd registry
   uv run --with jsonschema .github/workflows/build_registry.py
   python3 .github/workflows/verify_agents.py --auth-check --agent harn
   ```

2. Confirm `harn/agent.json` points at the latest release that includes the
   file-less ACP attach server and registry-recognized auth method. The first
   submission must match a live release; after listing, the registry
   auto-updates versions hourly from GitHub Releases.

3. Fork `agentclientprotocol/registry`, copy `harn/` to the repo root, and open
   a PR per [`CONTRIBUTING.md`][contrib]. Once merged, Harn is one-click
   installable in Zed (`Add Agent`) and JetBrains
   (*Settings → Tools → AI Assistant → Agents → Install From ACP Registry*).

[zed]: https://zed.dev/blog/acp-registry
[jb]: https://blog.jetbrains.com/ai/2026/01/acp-agent-registry/
[schema]: https://github.com/agentclientprotocol/registry/blob/main/agent.schema.json
[contrib]: https://github.com/agentclientprotocol/registry/blob/main/CONTRIBUTING.md
