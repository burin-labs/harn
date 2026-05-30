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
  the binary distribution (GitHub release tarballs for the five published
  targets), launching `harn serve acp`.
- [`harn/icon.svg`](harn/icon.svg) — 16×16 `currentColor` icon.

This is the agent's free editor discoverability listing. It is **not** the
signed skills/connectors marketplace (harn-cloud#24).

## Validation status

The manifest passes the registry's structural CI gates today. Run from a clone
of `agentclientprotocol/registry` with `harn/` copied in:

```bash
# Schema + ID + version + distribution + icon + URL accessibility — PASSES
uv run --with jsonschema .github/workflows/build_registry.py
# => "Added agent: harn v0.8.51", exit 0
```

All five release archive URLs return HTTP 200.

## Blocker: submission is gated on ACP runtime work (do not open the PR yet)

The registry's **auth gate** is the one check Harn does not yet pass. CI runs:

```bash
python3 .github/workflows/verify_agents.py --auth-check --agent harn
```

The validator launches the binary with exactly the manifest's `cmd` + `args`
(`./harn serve acp`, no extra arguments), sends an ACP `initialize`, and
requires the result's `authMethods` array to contain at least one method whose
type resolves to `agent` or `terminal` (a method with no explicit `type`
defaults to `agent`; see [`AUTHENTICATION.md`][auth]).

Two concrete gaps block this — both reproduced against the registry's own
tooling:

1. **Bare launch fails.** `harn serve acp` requires a positional `<FILE>`
   (the `.harn` agent to serve). The registry invokes the binary with no file,
   so the process exits with code 2 (`required arguments were not provided:
   <FILE>`) and never reaches the ACP loop. A stable, file-less ACP entrypoint
   is the scope of [#2664](https://github.com/burin-labs/harn/issues/2664).

2. **Empty `authMethods`.** With no `--api-key` / `--hmac-secret` flags (the
   bare launch), `build_auth_policy` produces an empty policy, so the
   `initialize` response carries `"authMethods": []` and fails the gate even if
   the process started. Harn's ACP auth methods also emit kind metadata under
   `_meta.harn` with ids `apiKey` / `hmac` / `oauth2`, not the registry's
   `agent` / `terminal` shape; the OAuth flow from
   [#2645](https://github.com/burin-labs/harn/issues/2645) /
   [#2639](https://github.com/burin-labs/harn/issues/2639) needs to surface a
   registry-recognized auth method.

Both are the explicit dependencies the issue sequences ahead of submission
(#2664 + #2639/#2645). Per that sequencing, this change lands the
ready-to-submit manifest and tracks the gate; it does **not** modify the
VM/runtime.

## Submitting (once #2664 + #2639/#2645 land)

After Harn returns a non-empty, registry-recognized `authMethods` from a
file-less `harn serve acp`:

1. Re-verify locally against a fresh clone of `agentclientprotocol/registry`:

   ```bash
   git clone https://github.com/agentclientprotocol/registry
   cp -R spec/acp-registry/harn registry/harn
   cd registry
   uv run --with jsonschema .github/workflows/build_registry.py
   python3 .github/workflows/verify_agents.py --auth-check --agent harn
   ```

2. Bump `version` (and the five `archive` URLs) in `harn/agent.json` to the
   release that ships the auth fix. The registry auto-updates versions hourly
   from GitHub Releases once listed, but the first submission must match a live
   release.

3. Fork `agentclientprotocol/registry`, copy `harn/` to the repo root, and open
   a PR per [`CONTRIBUTING.md`][contrib]. Once merged, Harn is one-click
   installable in Zed (`Add Agent`) and JetBrains
   (*Settings → Tools → AI Assistant → Agents → Install From ACP Registry*).

[zed]: https://zed.dev/blog/acp-registry
[jb]: https://blog.jetbrains.com/ai/2026/01/acp-agent-registry/
[schema]: https://github.com/agentclientprotocol/registry/blob/main/agent.schema.json
[contrib]: https://github.com/agentclientprotocol/registry/blob/main/CONTRIBUTING.md
[auth]: https://github.com/agentclientprotocol/registry/blob/main/AUTHENTICATION.md
