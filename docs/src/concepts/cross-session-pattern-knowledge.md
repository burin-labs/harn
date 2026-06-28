# Cross-session pattern knowledge

Harn owns cross-session pattern recall as a typed `harness.knowledge` layer over
`std/memory`. The concrete stdlib entrypoint is `std/agent/pattern_knowledge`,
which records completed-run observations, drafts reviewable skill proposals, and
loads accepted learned skills into future context.

This is not an A.5 session-store cross-session API. Session storage is useful
for transcript and run-event audit trails; pattern recall is long-lived agent
knowledge with its own retention, recall, and promotion policy.

## Decision criteria

| Criterion | Decision |
|---|---|
| Query and write rate | Write once per completed run observation, then query during context assembly or explicit review. The default backend stays deterministic and cheap because `std/memory` uses local BM25 unless a host selects vector or hybrid recall. |
| Retention | Keep the newest bounded observation set, soft-delete superseded pending proposals, and persist accepted skills as project artifacts. Imports write a migration marker so legacy host stores are consumed once. |
| Multi-tenant shape | Namespace and memory root isolate local projects today. Cloud agents can map the same namespace to tenant, organization, and project scopes without adding a Burin-specific host API. |
| Embedding versus keyword | The default clustering is deterministic lexical matching so tests and replay are stable. Hosts that need semantic recall can open the same memory namespace in vector or hybrid mode through `std/memory`. |

## Primitive shape

`std/agent/pattern_knowledge` stores `harn.pattern_learning.v1` records under the
`project/pattern-learning` namespace:

- `observation` records capture a redacted prompt, session id, tool sequence,
  and observation time.
- `pending` records hold reviewable proposals, usually learned skills.
- `state` records store enablement and temporary suppression after rejection.

Accepted proposals become `SKILL.md` files under the project skill root. Future
context assembly reads those skills through the same Harn module, so local IDE
agents and cloud agents share the same read/write path.

## Migration

Burin previously wrote Swift-owned JSONL stores under
`.harn/session-store/burin.agent_context.pattern_learning.*.jsonl`. On first
use, the Harn module imports those observation, pending, and state records into
`.harn/memory/project/pattern-learning/events.jsonl`, then writes
`.burin-session-store-migrated-v1.json` inside that namespace. Hosts should call
the Harn primitive on launch or first review instead of maintaining a parallel
store.

The migration is intentionally lazy: projects that never used the old store pay
no startup cost, and projects that did keep auditability because the imported
records become ordinary append-only memory events.
