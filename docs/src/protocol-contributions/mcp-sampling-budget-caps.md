# MCP RFC: per-call budget caps for `sampling/createMessage`

**Upstream repo:** [modelcontextprotocol/modelcontextprotocol][mcp]
**Status:** **Obsoleted upstream. Not filed as a SEP and will not be.**
Filed 2026-05-17 as
[MCP discussion #2736](https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/2736)
(Ideas - General) and narrowed 2026-07-03 after community feedback. On
2026-07-25, while preparing the SEP, we found that
[SEP-2577][sep-2577] deprecated the entire Sampling feature on
2026-05-15. Retained as a design record and as a process lesson; see
[Why this was not filed](#why-this-was-not-filed).
**Authors:** Kenneth Sinder
**Prototype:** [`experiments/mcp-sampling-budget-caps/`](https://github.com/burin-labs/harn/tree/main/experiments/mcp-sampling-budget-caps)
— a dependency-free Node script that still runs all four decision paths.
**Tracker:** [harn#5539](https://github.com/burin-labs/harn/issues/5539)

[mcp]: https://github.com/modelcontextprotocol/modelcontextprotocol
[sep-2577]: https://github.com/modelcontextprotocol/modelcontextprotocol/pull/2577

## Why this was not filed

Sampling is deprecated. [SEP-2577][sep-2577] ("Deprecate Roots, Sampling,
and Logging") merged 2026-05-15, deprecating the feature as of protocol
version `2026-07-28`, with earliest removal in the first revision
released on or after 2027-07-28. The spec now says new implementations
SHOULD NOT adopt Sampling and existing ones SHOULD migrate to
integrating directly with LLM provider APIs.

A Standards Track SEP adding fields to a feature scheduled for removal
would not attract a sponsor, and asking a maintainer to sponsor one would
have spent credibility for nothing.

**This also explains the maintainer silence better than bandwidth did.**
Discussion #2736 proposed adding surface area to Sampling. Two months
later the feature was deprecated. No maintainer was going to engage with
the thread on its own terms, and the ledger's general read — that
maintainer attention is the binding constraint — was the wrong diagnosis
for this thread specifically.

**Upstream's answer to the problem is more radical than ours.** The
proposal here treats the payer inversion (a server initiates, the client
pays) as a fact to be managed with limits and typed failures. Deprecation
removes the inversion instead: a client that calls the provider API
directly owns the budget natively and needs no protocol mechanism to
bound it. That is a legitimate resolution of the same problem, and a
simpler one.

Note also that the gap was never total. Sampling's Security
Considerations already said clients SHOULD implement rate limiting, and
`modelPreferences.costPriority` already existed as an advisory cost
signal. What was missing was enforcement plus a typed failure, which is
narrower than "no cost control exists at all."

### Process lesson

Check a feature's lifecycle state before designing a proposal that
extends it. The work here included a duplicate check against competing
proposals and a full read of the SEP process, but never a read of the
spec page for the feature being modified — where the deprecation notice
sits in a banner at the top. One fetch would have saved the whole
exercise. Any future filing against MCP, A2A, or ACP should start by
confirming the target surface is not deprecated, superseded, or mid-
restructure.

### What survives

The design analysis below is retained because the underlying problem
outlives this venue. Any protocol where one party initiates a model call
and another pays it has the same two gaps: no per-call bound, and no way
to distinguish a policy refusal from a transient failure. If a
replacement for Sampling appears, or if the same shape arises in ACP or
A2A, the separation of enforced limits from advisory intent and the
decision-basis record are the parts worth carrying over.

Two design criticisms of this RFC are worth recording alongside it, both
raised in review before it was abandoned:

1. **It is two proposals stapled together.** A typed stop reason (which
   fixes the genuinely broken thing) and a money envelope (which imposes
   a pricing catalog on clients and discloses client policy to servers)
   are separable. The smaller version — type the failure, keep limits
   entirely client-internal — loses only the deterministic shrink
   computation and dodges every structural objection.
2. **Cost is not a function of tokens and price alone.** `estimatedCost`
   as modelled here ignores prompt-cache state. Across a series of
   sampling calls sharing a prefix, substituting a cheaper model pays
   cold-cache input and can raise total spend, so "downgrade to fit the
   budget" is not reliably cheaper. Any future version needs the meter
   basis to express cache state, and should not mandate model
   substitution as a remedy.

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

## Filing path (moot, retained for reference)

The SEP mechanics below were researched on 2026-07-25 and are accurate as
of that date. They no longer apply to this proposal but are the reference
for any future MCP filing.

The submission is a pull request adding `seps/0000-<title>.md`, renamed to
the PR number once opened. Required sections are Preamble, Abstract of
roughly 200 words, Motivation, Specification, Rationale, Backward
Compatibility, Security Implications, and Reference Implementation, in
that order per the format used by merged SEPs. Insufficient motivation is
called out as grounds for outright rejection.

A sponsor is mandatory to move out of `Awaiting Sponsor`, and must be a
Core Maintainer or Maintainer from `MAINTAINERS.md`. Tag one or two whose
area fits rather than everyone, share the PR in the relevant Discord
channel, and ask in `#general` if nothing happens within two weeks. Six
months without a sponsor means `dormant`, which the process states
explicitly is not rejection and can be revived. `MAINTAINERS.md` assigns
no technical ownership areas, so mapping a proposal to a maintainer means
inferring from working-group roles.

A prototype is required before acceptance, and pseudocode or a design
document alone is insufficient. A conformance scenario plus a
`sep-NNNN.yaml` traceability file mapping every MUST and SHOULD is
required before `Final`, not before acceptance.

Worth knowing for calibration: `SEP-1539: Timeout Coordination`, the
closest structural analogue to this proposal — standardizing coordination
of a resource budget, in that case time — went **dormant** on 2026-06-26
for lack of a sponsor.

[sep-guidelines]: https://modelcontextprotocol.io/community/sep-guidelines

## Open questions (unresolved when the proposal was abandoned)

These were the four points the RFC intended to put to maintainers. They
are recorded because they are the questions any successor proposal in
another venue will face, not because an answer is still being sought.

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
