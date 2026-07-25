# Protocol filing status ledger

Last verified: 2026-07-25 UTC.

This ledger records direct upstream reads for the protocol-contribution
threads that back the RFCs in this directory. It is intentionally factual:
only record a state here after fetching the upstream discussion, PR, or
issue directly.

In [Diataxis](https://diataxis.fr/) terms, this is reference material.
It lists current facts and links; it is not a how-to guide or a design
explanation.

## Public follow-up posture

Upstream comments and PRs should use neutral ecosystem evidence, peer
behavior, and public prior art. Do not lead with Harn or Burin-specific
adoption claims. A local implementation can inform our internal
confidence, but public posts should stand on protocol semantics and
independently checkable examples.

One qualification, from reading the MCP SEP process on 2026-07-25: a
**prototype offered as feasibility evidence is not an adoption claim**,
and the SEP process explicitly requires one before acceptance
("pseudocode alone" and "a design document without code" are called out
as insufficient). Offering a runnable proof-of-concept is in-posture;
citing internal usage numbers as an argument is not.

## Every filing at a glance

| Thread | Filed | Maintainer engaged? | Third-party support | Ball is with |
|---|---|---|---|---|
| [ACP #1220](https://github.com/agentclientprotocol/agent-client-protocol/discussions/1220) `session/inject` | 2026-05-17 | Yes — invited the RFD | 3 independent | Maintainers (review of `#1261`) |
| [ACP #1261](https://github.com/agentclientprotocol/agent-client-protocol/pull/1261) inject RFD | 2026-05-19 | Not since 2026-06-27 rebase | 3 independent | Maintainers |
| [ACP #1224](https://github.com/agentclientprotocol/agent-client-protocol/discussions/1224) `session/remind` | 2026-05-17 | Yes — soft-parked 2026-07-06 | none | Maintainers (by their own request) |
| [ACP #1233](https://github.com/agentclientprotocol/agent-client-protocol/discussions/1233) `session/suspend` | 2026-05-17 | No | 1 (peer implementer) | **Us** — reply to peer feedback |
| [A2A #1857](https://github.com/a2aproject/A2A/discussions/1857) idempotency | 2026-05-17 | No | 1 | **Us** — posture decision |
| [A2A #1858](https://github.com/a2aproject/A2A/discussions/1858) `PAUSED` | 2026-05-17 | No | 3 | Maintainers (process question) |
| [A2A #2027](https://github.com/a2aproject/A2A/discussions/2027) `InjectTaskReminder` | 2026-07-03 | No | none | Nobody — cold |
| [A2A #2028](https://github.com/a2aproject/A2A/issues/2028) actor-chain | 2026-07-03 | No | 3 | **Us** — thread converged past our filing |
| [MCP #2736](https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/2736) budget caps | 2026-05-17 | No | 2 | **Us** — draft the SEP |
| [MCP #3007](https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/3007) `notifications/reminder` | 2026-07-03 | No | none | Nobody — cold |
| [MCP #3008](https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/3008) `authenticatedIdentity` | 2026-07-03 | No | 1 | **Us** — sponsor outreach |
| [oauth-wg #73](https://github.com/oauth-wg/oauth-identity-assertion-authz-grant/issues/73) actor chain | 2026-07-03 | **Yes — direct question to us** | n/a | **Us** — 3 weeks overdue |

Nothing has been rejected anywhere. The pattern across twelve threads is
consistent: proposals draw independent third-party support and no
maintainer verdicts. Maintainer attention, not proposal quality, is the
binding constraint — so the highest-value actions are the ones that do
not require a maintainer to move first.

## ACP

Context as of 2026-07-25: unchanged from the 2026-07-03 read. The v2 RFD
collection is the most active area of the repo, but every v2 RFD PR
merged since 2026-06-01 was maintainer-authored. Externally-authored RFD
PRs (including `#1261`) sit in review while their content gets absorbed
into `unstable-v2` commits. Plan for discussions to be the unit of
influence, not PR merges. Two v2 docs matter for our filings: the merged
session-resume-replay RFD (unified `session/load` + `session/resume`,
optional `replayFrom` cursor) is the substrate any `session/suspend`
follow-up must sit on, and open PR
[`#1237`](https://github.com/agentclientprotocol/agent-client-protocol/pull/1237)
(client-provided system prompt) overlaps the `session/remind` territory
and should be differentiated against, not ignored.

| Item | Verified state | Notes |
|---|---|---|
| [`agentclientprotocol/agent-client-protocol#1220`](https://github.com/agentclientprotocol/agent-client-protocol/discussions/1220) | Open discussion, 4 comments. | A maintainer invited an RFD; the follow-up RFD is PR [`#1261`](https://github.com/agentclientprotocol/agent-client-protocol/pull/1261). Two third-party comments since: `point-source` (Kandev, an ACP client running multiple agents, 2026-07-10) endorses the `queue`/`steer` split and specifically wants agents to advertise `session.inject.modes` so clients can gate UI per agent instead of hardcoding assumptions; `ofekron` (2026-07-20) argues acceptance, delivery, and application are three distinct states — for `queue`, acceptance should mean durably recorded, not model-visible; for `steer`, delivery means the input crossed a declared safe breakpoint and application means the continuation turn incorporated it. |
| [`agentclientprotocol/agent-client-protocol#1261`](https://github.com/agentclientprotocol/agent-client-protocol/pull/1261) | Open PR, mergeable, `REVIEW_REQUIRED`, +357/-0. No maintainer review at any point; none since the 2026-06-27 rebase. | The only review is `SteffenDE` (2026-05-21), self-described as non-authoritative, who also closed their overlapping `promptQueueing` PR in favor of waiting for v2. Two production adoption reports arrived unprompted: `xxchan` (repo CONTRIBUTOR, [Raft](https://raft.build), 2026-07-10) states the absence of steering "is currently an adoption blocker for ACP" for a multi-agent platform where users send corrections from another client mid-execution; `ChrisAkre` (2026-07-19) implemented a Copilot-SDK→ACP bridge with `session/inject` for multi-client fanout and reports it "an utterly massive quality of life improvement." |
| [`agentclientprotocol/agent-client-protocol#484`](https://github.com/agentclientprotocol/agent-client-protocol/pull/484) | Closed in favor of `#1261`. | Relevant predecessor for prompt queueing / steer-via-yield framing. |
| [`agentclientprotocol/agent-client-protocol#1224`](https://github.com/agentclientprotocol/agent-client-protocol/discussions/1224) | Open discussion. **Maintainer replied 2026-07-06** — supersedes the previous "no maintainer response" state. | `benbrandt` (MEMBER) answered the 2026-06-27 ping: "let me revisit in a bit. We also had a lot of drafts and so I have been trying to churn through those so we could actually make progress on some more." Soft-parked behind v2 draft triage, explicitly not declined. Do not re-ping; the maintainer has asked for time. |
| [`agentclientprotocol/agent-client-protocol#1233`](https://github.com/agentclientprotocol/agent-client-protocol/discussions/1233) | Open discussion, no maintainer reply. **New third-party feedback 2026-07-20.** | `ofekron`, disclosing that they maintain [Better Agent](https://github.com/ofekron/better-agent) (durable provider sessions, operator approvals, restart recovery), endorses the reduced v2 shape and asks that `session/suspend` be an acknowledged request rather than a notification, with an observable `requested → quiescing → suspended` state machine. This is new substance from a peer rather than a maintainer, so a reply is in-posture. Our 2026-07-03 v2 alignment comment ([`discussioncomment-17525582`](https://github.com/agentclientprotocol/agent-client-protocol/discussions/1233#discussioncomment-17525582)) stands. |
| [`agentclientprotocol/registry#397`](https://github.com/agentclientprotocol/registry/pull/397) | **Merged.** Harn is listed in the ACP registry. | Supersedes the previous "open, no review decision" state. Entry refreshed to Harn v0.9.15 before merge, with `build_registry.py --dry-run` and `verify_agents.py --auth-check` validation posted. |
| [`agentclientprotocol/registry#460`](https://github.com/agentclientprotocol/registry/pull/460) | Closed by us, 2026-07-25. | A fork-local documentation convention (`CLAUDE.md` → `AGENTS.md` symlink) opened against upstream by mistake. No upstream signal; recorded only so the closure is not re-investigated. |

## A2A

Context as of 2026-07-25: unchanged from the 2026-07-03 read. A2A v1.0.0
shipped 2026-03-12 (v1.0.1 on 2026-05-28), which predates our 2026-05-17
filings. v1.0 renamed operations to PascalCase across all bindings
(`message/send` → `SendMessage`, subscription → `SubscribeToTask`), moved
enums to `SCREAMING_SNAKE_CASE` per ProtoJSON, removed `kind`
discriminators, and made `a2a.proto` the normative source of truth. Both
filed discussions and the RFCs in this directory originally used pre-1.0
naming; the RFCs were revised to v1.0 conventions on 2026-07-03, and any
follow-up upstream post should use v1.0 names.

Extension mechanics, read 2026-07-25: extensions are declared as
`AgentExtension` entries under `AgentCapabilities` (URI, description,
required flag, params) and activated by `A2A-Extensions` header
negotiation, with the agent echoing back what it activated. Four
categories exist — data-only, profile, method, and state-machine
extensions. Only the canonical `https://a2a-protocol.org/extensions/`
prefix is reserved for official extensions under `a2aproject` governance;
**community extensions may be self-published under their own URI**, with
permanent-identifier services such as w3id.org recommended. A new URI
MUST be used for any breaking change to an extension's logic, data
structures, or required params. This matters for `#1858` and `#2028`:
both have a path that does not depend on a TSC answer.

| Item | Verified state | Notes |
|---|---|---|
| [`a2aproject/A2A#1857`](https://github.com/a2aproject/A2A/discussions/1857) | Open discussion, 1 comment, **no activity since 2026-05-18** — the coldest of our filings. | Idempotency keys + post-cancel state semantics. `chopmob-cloud` endorsed from production and recommended accepting the key in both the request body and a Stripe-style `Idempotency-Key` HTTP header, key opaque to A2A with a server-chosen window. Still titled with pre-v1.0 `tasks/send`, and the only filing with **no RFC source doc in this directory**. Posture decision tracked in [harn#5540](https://github.com/burin-labs/harn/issues/5540). |
| [`a2aproject/A2A#1858`](https://github.com/a2aproject/A2A/discussions/1858) | Open discussion, 12 comments, no maintainer/TSC reply. Unchanged since 2026-06-27. | Community feedback converged on one `PAUSED` state plus a structured `pause` object (`initiatedBy`, `pausedUntil` lease, `resumeToken`) rather than separate enum values, plus an opaque `lastSideEffectRef` whose absence must not be read as proof that no side effect occurred. The draft-PR-vs-extension ping ([`discussioncomment-17455657`](https://github.com/a2aproject/A2A/discussions/1858#discussioncomment-17455657)) is still unanswered. Given the extension mechanics above, self-publishing is an option if the thread stays cold. |
| [`a2aproject/A2A#1937`](https://github.com/a2aproject/A2A/issues/1937) | Open issue, last updated 2026-06-19. | Context-binding profile for delegated authority — binds an already-valid delegation to a task/session/target/scope. Complement of (not substitute for) the [actor-chain extension RFC](./a2a-actor-chain-extension.md); best anchor thread for that filing. |
| [`a2aproject/A2A#153`](https://github.com/a2aproject/A2A/issues/153) | Open issue (since 2025-06). | Confused-deputy framing for A2A; canonical motivation citation for payload-visible principals. |
| [`a2aproject/A2A#2027`](https://github.com/a2aproject/A2A/discussions/2027) | Filed 2026-07-03 (Ideas category). **Zero comments after 22 days.** | `InjectTaskReminder` ambient-context discussion, from the [reminder RFC](./a2a-message-kind-reminder.md) with A2A v1.0 naming. Dupe-checked before filing. Cold-start with no venue warming; see [harn#1829](https://github.com/burin-labs/harn/issues/1829). |
| [`a2aproject/A2A#2028`](https://github.com/a2aproject/A2A/issues/2028) | Filed 2026-07-03. Open issue, 4 comments, active through 2026-07-22, no maintainer. **The thread has converged on a sharper model than we filed.** | Actor-chain extension, anchored to `#1937` / `#153`. `0xbrainkid` wants each hop to carry `sub` + session/nonce binding + `scopes`, not a bare subject. `giskard09` added **monotonic narrowing** — each hop's scopes a subset of its predecessor's — as a mechanically checkable invariant that makes the confused-deputy case from `#153` detectable without a cross-hop log join. `aeoess` then supplied the necessary correction: since `actorChain` is caller-supplied, a fabricated chain can narrow perfectly, so narrowing is a **well-formedness** property only; proof of grant requires a per-hop `proof_ref` an outside verifier resolves without trusting the caller's payload. `giskard09` agreed these are two separate properties that the extension text should state separately. This split matches what the IETF drafts leave unstandardized (see below) — independent convergence, worth adopting verbatim rather than re-deriving. |

## MCP and OAuth Identity

### MCP process facts (read 2026-07-25)

Recorded here because both open MCP items are process-blocked rather than
substance-blocked, per the
[SEP guidelines](https://modelcontextprotocol.io/community/sep-guidelines):

- A SEP is a **PR** adding `seps/0000-title.md`, renamed to the PR number
  once opened — not an issue and not a discussion.
- Required sections: Preamble, Abstract (~200 words), Motivation,
  Specification, Rationale, Backward Compatibility, Reference
  Implementation, Security Implications. Insufficient motivation is
  called out as grounds for outright rejection.
- **A sponsor is mandatory** to move `Awaiting Sponsor` → `draft`: a Core
  Maintainer or Maintainer from `MAINTAINERS.md`. Tag 1–2 relevant
  maintainers, not everyone; if no response in two weeks, ask in
  `#general` on Discord.
- No sponsor within **6 months** means `dormant`, which the process
  states explicitly is *not* rejection and is revivable.
- Discussing with the relevant working/interest group on Discord first is
  described as "the single best way to refine your proposal and build
  early support," and a cold PR submission is the weakest entry point.
- A **prototype is required before acceptance** (not before submission);
  pseudocode or a design document alone is insufficient.
- Standards Track SEPs with observable protocol behavior additionally
  need a merged conformance scenario plus a `sep-NNNN.yaml` traceability
  file mapping every MUST/SHOULD before `Final` — not before acceptance.

### IETF draft facts (read 2026-07-25)

The actor-chain gap has **partly resolved upstream in our favour.** The
representation half is being standardized; the semantic half is
explicitly out of scope, and that is the half we have implementation
experience in.

- **ID-JAG** (`draft-ietf-oauth-identity-assertion-authz-grant`) is at
  **revision 04, 2026-05-21**, an active OAuth WG draft with no intended
  RFC status set. `actor_token` is now `OPTIONAL` and permitted, but the
  draft deliberately stops there: "This specification does not define
  normative processing requirements for actor_token or whether an act
  claim is included in the issued ID-JAG." It defers to profiles.
  This supersedes our original premise that ID-JAG "explicitly disables
  `actor_token`," which was true of an earlier revision.
- **The Actor Profile** (`draft-mcguinness-oauth-actor-profile`,
  **2026-04-30**, expires 2026-11-01) is that profile, and covers more
  than we assumed. Multi-hop chains are specified: "Delegation chains
  MUST be represented by nesting `act` objects... the outermost `act`
  object identifies the immediate actor; inner `act` objects represent
  prior actors." Implementations "SHOULD support a local maximum of at
  least depth 4." Preservation is mandated more strongly than we asked
  for: "The AS MUST NOT silently drop an inbound `act` claim; if it
  cannot preserve or extend the chain, it MUST reject the request." The
  canonical actor identifier is the (`act.iss`, `act.sub`) **pair**, not
  `act.sub` alone.
- **What the profile explicitly declines to standardize**, and therefore
  remains open: (1) per-hop scope narrowing — "This document does not
  standardize the policies by which systems determine whether a given
  actor is permitted to act for a subject," scope reduction is
  deployment-specific; (2) per-hop proof-of-possession — "Other members
  carried inside an `act` object... do not have standardized
  proof-of-possession semantics," only the top-level `cnf` conveys the
  current presenter's key.

| Item | Verified state | Notes |
|---|---|---|
| [`modelcontextprotocol/modelcontextprotocol#2736`](https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/2736) | Open discussion, 4 comments, no maintainer response at any point. **The 2026-07-03 narrowed-scope restatement has drawn no objection in 22 days.** | Per-call sampling budget caps. `ralftpaw` separated host policy limits (hard caps enforced regardless of server request) from server-declared budget intent. `HarperZ9` endorsed the SEP path with a deliberately small first version — one host-owned limit envelope plus one typed stop/failure shape — and argued the load-bearing field is the decision basis (estimated cost, policy limit applied, meter basis) rather than `max_cost_usd`. Our restatement offered to draft the SEP unless maintainers objected. Next step is unblocked and ours; tracked in [harn#5539](https://github.com/burin-labs/harn/issues/5539). |
| [`modelcontextprotocol/modelcontextprotocol#3007`](https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/3007) | Filed 2026-07-03 (Ideas - General). **Zero comments after 22 days.** | `notifications/reminder` server→host ambient-context discussion, from the [reminder RFC](./mcp-notifications-reminder.md). Dupe-checked before filing. Same cold-start pattern as A2A `#2027`; the reminder primitive has no natural WG home, which is the likely cause. |
| [`modelcontextprotocol/modelcontextprotocol#3008`](https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/3008) | Filed 2026-07-03 (Ideas - General). One supportive third-party reply 2026-07-12; no maintainer. | `authenticatedIdentity` pre-SEP discussion, from the [identity RFC](./mcp-authenticated-identity.md). `tamish560` confirms the gap from experience: the "connected as" question is unanswerable today without per-server knowledge of which tool returns user info, and `InitializeResult` is the right slot because the server already knows who authorized the session. Progression is sponsor-gated; that outreach is unstarted. |
| [`modelcontextprotocol/modelcontextprotocol#214`](https://github.com/modelcontextprotocol/modelcontextprotocol/issues/214) | Closed. | Maintainer guidance on 2026-01-16 pointed custom auth pieces toward [`modelcontextprotocol/ext-auth`](https://github.com/modelcontextprotocol/ext-auth). |
| [`modelcontextprotocol/ext-auth#13`](https://github.com/modelcontextprotocol/ext-auth/issues/13) | Open, still no activity since 2026-01-31. | Maintainer response says Enterprise-Managed Authorization does not currently support distinguishing agent vs user identity and points to ID-JAG issue `#73`. |
| [`oauth-wg/oauth-identity-assertion-authz-grant#73`](https://github.com/oauth-wg/oauth-identity-assertion-authz-grant/issues/73) | Open. **A draft author asked us a direct question on 2026-07-03; it has been unanswered for 22 days.** | The live venue for actor-chain work. Our implementer feedback was posted 2026-07-03 ([`issuecomment-4878092226`](https://github.com/oauth-wg/oauth-identity-assertion-authz-grant/issues/73#issuecomment-4878092226)) per the [positioning note](./oauth-actor-chain-positioning.md). `mcguinness` replied the same day asking whether we had reviewed [`draft-mcguinness-oauth-actor-profile`](https://datatracker.ietf.org/doc/draft-mcguinness-oauth-actor-profile/) and noting ID-JAG's rules were relaxed to allow `actor_token` on the token exchange request. This is the oldest outstanding debt on the board and the only one where a spec author is waiting on us. |
| [`oauth-wg/oauth-identity-assertion-authz-grant#80`](https://github.com/oauth-wg/oauth-identity-assertion-authz-grant/issues/80) | Closed as completed and milestoned 2026-04-22. | Optional `actor_token` proposal split out from `#73`; folded into the `#73` direction rather than rejected, and now carried by the Actor Profile draft. |
| [`modelcontextprotocol/modelcontextprotocol#1299`](https://github.com/modelcontextprotocol/modelcontextprotocol/issues/1299) | Closed as completed 2025-09-02. | SEP-1299 is server-side OAuth flow management, unrelated to a server→client identity surface; it does not claim the `authenticatedIdentity` slot. |
| [`modelcontextprotocol/modelcontextprotocol` discussion `#1827`](https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/1827) | Open discussion, unanswered (opened 2025-11-17). | `upstream_identity` propagation, client→server — the opposite direction from the [`authenticatedIdentity` RFC](./mcp-authenticated-identity.md); the two compose. |

## Local follow-up candidates

Ordered by whether they depend on someone else moving first.

**Unblocked — no maintainer required:**

- **oauth-wg `#73`**: answer `mcguinness`. The reply should confirm the
  nested-`act` representation is what we want and adopt it, then state
  the two things the profile leaves open (per-hop scope narrowing;
  per-hop evidence) as the layer above it, citing the independent
  convergence on the same split in A2A `#2028`. Prerequisite for
  accuracy: the E4 draft-watch note ([harn#3347](https://github.com/burin-labs/harn/issues/3347)).
- **MCP `#2736`**: draft `seps/0000-sampling-budget-caps.md` per the
  process facts above. Sponsor outreach should follow the Discord-first
  guidance rather than a cold PR. Tracked in
  [harn#5539](https://github.com/burin-labs/harn/issues/5539).
- **A2A `#2028`**: post a consolidating restatement adopting the
  narrowing-versus-`proof_ref` split as two separately stated
  properties, while the thread is still warm.
- **ACP `#1233`**: reply to `ofekron` accepting the acknowledged-request
  framing and the `requested → quiescing → suspended` state machine,
  and state the quiescence guarantee in wire terms. Peer feedback, not a
  maintainer ping.
- **MCP `#3008`**: begin auth-area sponsor outreach via the relevant
  WG/IG rather than waiting for the discussion to attract one.
- **`#3347`**: write the IETF draft-watch note; three of five tracked
  drafts are still unpinned (WIMSE WIT/WPT, transaction-tokens-for-agents,
  `draft-klrc-aiagent-auth`).
- **Audit item from the Actor Profile read**: the canonical actor
  identifier is the (`act.iss`, `act.sub`) pair. Anywhere our internal
  chain keys an actor by subject alone is a latent interop bug; check
  against Epic A / C under
  [harn#3326](https://github.com/burin-labs/harn/issues/3326).

**Blocked on maintainers — hold:**

- **ACP `#1261`**: keep conflict-free against upstream `main`, keep the
  public framing anchored in existing editor/agent behavior. Three
  unprompted third-party adoption reports now sit in the thread; that is
  the argument, and it does not need restating by us.
- **ACP `#1224`**: the maintainer asked for time on 2026-07-06. Do not
  re-ping.
- **A2A `#1858`**: draft PR stays ready-to-cut but uncut until the
  draft-PR-vs-extension question is answered — or until we decide to
  self-publish under our own extension URI.
- **A2A `#2027`, MCP `#3007`**: cold with no venue warming. Leave parked
  or warm the venue first; no re-pings without new substance.

**Standing rule:** no re-pings without new substance. Third-party
feedback arriving in a thread *is* new substance; the passage of time is
not.

## Not yet filed

- **`session/inject_host_event`** — documented in
  [typed host-event injection](./acp-session-inject-host-event.md) as a
  shipped Harn extension, the only doc in this directory with no upstream
  filing and no tracking issue. It is the host-originated sibling of
  `session/inject`: same delivery seams, opposite initiator. Natural
  follow-on RFD if `#1261` lands; premature while `#1261` is unreviewed.
- **MCP suspend/resume** — deliberately not filed. MCP tools are
  request/response with no agent-lifecycle surface to extend. Recorded so
  the absence is not mistaken for an oversight.
