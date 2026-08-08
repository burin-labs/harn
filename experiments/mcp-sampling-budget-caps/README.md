# MCP sampling budget caps: prototype

Prototype for the per-call sampling budget-caps proposal filed as
[modelcontextprotocol/modelcontextprotocol#2736][disc]. The RFC it backs
is [MCP RFC: per-call budget caps for `sampling/createMessage`][rfc].

```sh
node poc.mjs
```

No dependencies, no network, no API keys. Exits non-zero if any assertion
fails.

## Why this exists

The MCP SEP process requires a runnable prototype before a proposal can
be accepted, and states that pseudocode or a design document alone will
not do. This is that artifact. It is deliberately one file so a reviewer
can read it and run it without setting up a toolchain.

## What it demonstrates

A stub model prices tokens from a fixed rate card, so every number the
script prints is reproducible.

1. A server may declare advisory budget intent, and the host's own policy
   limit wins regardless. Scenario 2 has the server asking for `$2.00`
   against a `$0.05` host limit and still being refused.
2. A pre-flight refusal returns `stopReason: "budget_exceeded"` with
   empty content and no recorded actual cost, because no tokens were
   spent.
3. A mid-generation stop returns `stopReason: "budget_exhausted"`, keeps
   the partial content, and records an actual cost within the limit.
4. A transport failure stays a JSON-RPC error with no budget decision
   attached.

Case 4 carries the argument. Today all four of these reach the server as
an indistinguishable error, so a server cannot tell a policy refusal
(retrying is guaranteed to fail) from a transient timeout (retrying is
correct).

The script also shows what a server does with each outcome. On a refusal
it reads the decision basis and computes how far to shrink its request:
a `$0.1027` estimate against a `$0.05` limit yields "retry at ~48% of
input" rather than a blind retry.

## Scope

Throwaway prototype, not a Harn feature and not wired into the build. It
models the host and server as direct calls across the decision boundary
rather than framing real JSON-RPC, because the proposal concerns what the
host decides and what the result carries, not transport.

[disc]: https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/2736
[rfc]: ../../docs/src/protocol-contributions/mcp-sampling-budget-caps.md
