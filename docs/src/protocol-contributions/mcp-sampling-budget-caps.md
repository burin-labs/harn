# MCP RFC: per-call budget caps for `sampling/createMessage`

**Upstream repo:** [modelcontextprotocol/modelcontextprotocol][mcp]
**Status:** Filed 2026-05-17 as
[MCP discussion #2736](https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/2736)
(Ideas - General). Narrowed on 2026-07-03 after community feedback, with
an offer to draft the SEP absent maintainer objection. No maintainer
response as of 2026-07-25 and no objection to the narrowed scope in the
22 days since. SEP not yet submitted; drafting tracked under
[harn#5539](https://github.com/burin-labs/harn/issues/5539).
**Authors:** Burin Labs
**Prototype:** [`experiments/mcp-sampling-budget-caps/`](https://github.com/burin-labs/harn/tree/main/experiments/mcp-sampling-budget-caps)
— a dependency-free Node script that runs all four decision paths.

[mcp]: https://github.com/modelcontextprotocol/modelcontextprotocol

## Problem statement

`sampling/createMessage` inverts who pays. A server asks the host to run
a model call, and the host's credentials and budget cover it. The host is
the party with a spend policy and the party with no way to express one.

Two gaps follow from that.

The host cannot bound a single call. It can refuse the request outright,
or run it and hope, or invent a private convention that only servers it
already knows about will honor. `maxTokens` bounds output length, which
is a poor proxy: a long cheap completion and a short expensive one both
pass the same limit, and neither the input side nor the model's price
enters the calculation.

When the host does intervene, the server cannot tell what happened. A
policy refusal, a model error, a transport timeout, and a content filter
all arrive as an error with prose in it. That difference matters for the
one decision a server makes next. Retrying a transient timeout is
correct. Retrying a policy refusal produces the identical refusal, and a
server with a retry loop will spend the host's remaining budget
discovering that.

Neither gap is exotic. Any host running untrusted or semi-trusted MCP
servers hits both on the first call it wants to decline.

## Keep two things separate

The first substantive comment on the thread, from `ralftpaw` on
2026-05-19, made a distinction the original filing had blurred, and it
belongs in the design rather than the discussion:

*Host policy limits* are what the client, user, or administrator
enforces. They are not negotiable and the server does not get a vote.

*Server-declared budget intent* is what the server believes the call
needs. It is advisory. A host may use it to reject early, or ignore it.

Conflating them produces an API where a server that asks for a two-dollar
budget can reasonably believe it has been granted one. Naming them
separately makes the asymmetry legible: intent is a request, limits are a
decision.

## Proposed shape

Three additions, all optional, none breaking.

### 1. Advisory intent on the request

```json
{
  "method": "sampling/createMessage",
  "params": {
    "messages": [{ "role": "user", "content": { "type": "text", "text": "..." } }],
    "maxTokens": 2048,
    "budget": {
      "intent": { "maxCost": { "amount": "0.05", "currency": "USD" } },
      "onExceeded": "reject"
    }
  }
}
```

A server that omits `budget` behaves exactly as it does today. The
`onExceeded` field lets a server say whether it would rather be refused
before the call or truncated during it, which is a genuine difference for
a server assembling a document versus one answering a yes-or-no question.
The host is free to disregard it.

### 2. Host limits, enforced and reported

The host never puts its limits in the request; the request is the
server's. Limits surface in the result, as part of the record of what was
decided. A host that wants servers to know its ceiling before asking can
advertise a coarse figure at `initialize`, which is question 2 below.

### 3. Typed stop reason and decision basis on the result

```json
{
  "role": "assistant",
  "content": { "type": "text", "text": "" },
  "model": "example-model-v2",
  "stopReason": "budget_exceeded",
  "budget": {
    "decision": {
      "estimatedCost": { "amount": "0.1027", "currency": "USD" },
      "limitApplied": { "amount": "0.0500", "currency": "USD" },
      "limitSource": "host_policy",
      "meterBasis": "example-model-v2@2026-07-01:in=3.00/Mtok,out=15.00/Mtok",
      "estimatedTokens": { "input": 24000, "output": 2048 }
    }
  }
}
```

Two stop reasons, because pre-flight and mid-generation are different
events with different recovery:

`budget_exceeded` means the host declined before calling the model. No
tokens were spent. Content is empty.

`budget_exhausted` means the host stopped a call in progress. Partial
content is present and is as valid as any other truncated completion.

Both are distinct from `maxTokens` and from any model-side stop reason. A
server matching on `stopReason` gets a mechanical answer to "was this my
fault, the model's, or the host's policy?"

### Why the decision basis is the load-bearing part

`HarperZ9` argued on 2026-06-27 that the interesting field is not
`maxCost` but the basis for the decision, and I think that is right. It
changes what a server can do with a refusal.

Given only `budget_exceeded`, a server can retry blindly or give up.
Given the estimate, the limit, and the meter basis, it can compute how
much smaller its request needs to be and shrink deterministically on the
next attempt. The 0.1027 estimate against a 0.05 limit in the example
above tells the server to retry at roughly 48% of its input, which is an
action rather than a guess. Scenario 2 of the prototype computes exactly
that number, so the arithmetic here is checkable rather than asserted.

`meterBasis` is a string the host chooses, and the RFC deliberately does
not standardize its grammar. It exists so an audit reader can tell
whether two decisions were priced the same way. Hosts that price from a
vendor rate card, a negotiated contract, or a local token count all
produce different bases, and none of them is wrong.

## What the first version leaves out

`HarperZ9`'s framing was to keep the first version deliberately small,
and the following are all defensible features that would sink it:

Currency conversion and FX. Amounts are decimal strings with an explicit
currency, and a host that cannot price a request in the currency a server
named should refuse rather than convert.

Session pools and cross-call budgets. Per-call is the smaller problem and
the one with an obvious enforcement point. A pool needs lifecycle rules,
which is a separate proposal.

Negotiation. No round-trip where a server counters the host's limit. A
server that wants a smaller call can send one.

Picking a meter. Both token-based and cost-based limits stay expressible.
Choosing on the server's behalf is the kind of decision that generates
two years of argument for no gain.

## Prototype

The SEP process requires a runnable prototype before acceptance, and
states that pseudocode or a design document alone will not do. The
prototype lives at `experiments/mcp-sampling-budget-caps/` and runs with
`node poc.mjs`, with no dependencies and no API keys, because the fastest
way to lose a reviewer is to ask them to install a toolchain first.

It models the host and server sides as direct calls across the decision
boundary rather than framing real JSON-RPC, since the proposal is about
what the host decides and what the result carries, not about transport.
A stub model prices tokens from a fixed rate card. The script asserts the
four paths a host takes: under budget, refused pre-flight, truncated
mid-generation, and a transport failure that must not be reported as a
budget decision. The last case is the one worth having a test for, since
telling those apart is the whole point.

## Filing path

Per the [SEP guidelines][sep-guidelines], read 2026-07-25:

The submission is a pull request adding `seps/0000-sampling-budget-caps.md`,
renamed to the PR number once opened. It is Standards Track, and the
required sections are Preamble, Abstract of roughly 200 words,
Motivation, Specification, Rationale, Backward Compatibility, Reference
Implementation, and Security Implications. Insufficient motivation is
called out as grounds for outright rejection, which is why the problem
statement above leads with the payer inversion rather than the field
list.

A sponsor is mandatory to move out of `Awaiting Sponsor`, and must be a
Core Maintainer or Maintainer from `MAINTAINERS.md`. The guidance is to
tag one or two whose area fits, share the PR in the relevant Discord
channel, and ask in `#general` if nothing happens within two weeks. Six
months without a sponsor means `dormant`, which the process states
explicitly is not rejection and can be revived.

The discussion-first step the guidelines recommend is already served by
discussion #2736, public since 2026-05-17, which drew two substantive
technical replies. Filing the PR does not require the sponsor to be lined
up first: sponsorship is step 4 of the documented flow, after the PR
exists at step 2.

A conformance scenario plus a `sep-NNNN.yaml` traceability file mapping
every MUST and SHOULD is required before `Final`, not before acceptance.

[sep-guidelines]: https://modelcontextprotocol.io/community/sep-guidelines

## Open questions for upstream maintainers

1. **Does MCP want a cost unit at all?** Cost requires a price source the
   protocol does not own and cannot verify. Tokens are verifiable and
   useless for the actual problem, since token count does not tell you
   what a call costs across models. The RFC proposes allowing both and
   letting hosts declare which they used, but a maintainer may prefer
   tokens only.
2. **Should hosts advertise a ceiling at `initialize`?** It would let
   servers size requests before asking, at the cost of leaking host
   policy to every server that connects, including ones that will use it
   to ask for exactly the maximum.
3. **Result with a stop reason, or JSON-RPC error?** A result preserves
   partial content for `budget_exhausted`, which an error cannot. Using a
   result for the pre-flight refusal too keeps one code path, at the cost
   of returning a success-shaped response for a call that never ran.
4. **Is per-call the right granularity?** Several hosts will want a
   session pool. Per-call is proposed because it has one obvious
   enforcement point, but if maintainers see pools as the real
   requirement, the field layout should anticipate them.

## References

- [MCP discussion #2736 — the filed thread](https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/2736)
- [SEP guidelines][sep-guidelines]
- [MCP design principles](https://modelcontextprotocol.io/community/design-principles)
- [Filing status ledger](./status-ledger.md)
- [harn#5539 — SEP drafting tracker](https://github.com/burin-labs/harn/issues/5539)
