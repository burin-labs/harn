# Profile bulletins

`std/personas/bulletins` is Harn's typed envelope for proposing durable
persona/user/project/team facts. Bulletins make persona context auditable and
reviewable rather than hidden prompt mutation: an agent emits a
`harn.profile_bulletin.v1` proposal, the host decides whether to accept it, and
both events stay in the EventLog for replay.

```harn
import {
  bulletin_propose,
  bulletin_emit,
  bulletin_accept,
  bulletin_render_for_prompt,
} from "std/personas/bulletins"

let bulletin = bulletin_propose(
  {
    scope: "user",
    scope_key: "kenneth@example.com",
    subject: "kenneth",
    persona: "burin_home",
    context_key: "preferences",
    assertion: "prefers concise responses without trailing summaries",
    confidence: 0.92,
    source: {agent: "burin_home_curator", workflow: "memory_review"},
    evidence: [
      {kind: "user_msg", ref: "msg-42", excerpt: "stop summarizing"},
    ],
    privacy: {sync: "local_only"},
  },
)
let proposal = bulletin_emit(bulletin)
let decision = bulletin_accept(bulletin, {decided_by: "user"})
```

## Envelope

`bulletin_propose(input, options?)` returns `harn.profile_bulletin.v1`:

| Field | Purpose |
|---|---|
| `id` | Stable hash derived from `(scope, scope_key, subject, persona, assertion)` |
| `scope` | `user`, `project`, `workspace`, `task`, `team`, `session`, or `global` |
| `scope_key` | Stable target identifier (e.g. email, repo slug, task id) |
| `subject` | Display label for the fact (e.g. `"kenneth"`, `"harn"`) |
| `persona` | Optional persona name this bulletin is bound to |
| `context_key` | Optional context tag such as `"preferences"` or `"constraints"` |
| `assertion` | The durable fact text |
| `status` | `proposed` (default), `accepted`, `rejected`, `expired`, or `superseded` |
| `confidence` | Number in `[0, 1]`; out-of-range or non-numeric values throw |
| `evidence` | List of `{kind, ref, label?, excerpt?}` provenance pointers |
| `source` | `{agent?, workflow?, persona?, run_id?, task?}` proposing context |
| `privacy` | `{sync, redacted, contains_sensitive, redaction_hints, flags}` |
| `proposed_at` | ISO timestamp (defaults to `date_now_iso()`) |
| `expires_at`, `review_after` | Optional ISO timestamps for host TTL/review prompts |
| `supersedes` | Optional list of prior bulletin ids this proposal would replace |

`scope`, `scope_key`, `subject`, `assertion`, `confidence`, and `proposed_at`
are required. `privacy.sync` defaults to `host_default` and must be one of
`local_only`, `host_default`, `tenant`, or `shared`.

## Decisions

Hosts review proposals and emit decisions on the
`personas.bulletins.decisions` topic. Each decision is itself an auditable
record (`harn.profile_bulletin_decision.v1`) so the proposal/decision history
is replayable.

| Helper | Action |
|---|---|
| `bulletin_accept(bulletin, options?)` | Decision `accept`; status becomes `accepted` |
| `bulletin_reject(bulletin, options?)` | Decision `reject`; status becomes `rejected` |
| `bulletin_expire(bulletin, options?)` | Decision `expire` for TTL or staleness |
| `bulletin_supersede(bulletin, supersedes, options?)` | Replaces prior bulletin ids |
| `bulletin_decide(bulletin, action, options?)` | Build a typed decision envelope without emitting |
| `bulletin_emit_decision(decision, options?)` | Emit a prebuilt decision |

`bulletin_supersede` requires at least one prior bulletin id; the helpers
accept `decided_by` (`"host"`, `"user"`, `"policy"`, …), `decided_at`,
`rationale`, and a custom `topic`.

## Prompt context

Bulletins must never silently enter context as durable fact:

- `bulletin_emit` always writes status `proposed`, regardless of any
  caller-provided override.
- `bulletin_active(bulletins, now?)` returns only `accepted` bulletins that
  have not passed `expires_at` — proposals are excluded.
- `bulletin_render_for_prompt(bulletins, options?)` renders accepted facts and
  proposals under separate headings. The proposed section is labeled
  `"Proposed (pending review — do not treat as fact)"` so models can see the
  boundary. Pass `{include_proposed: false}` to drop proposals entirely.

```harn,ignore
let prompt_text = bulletin_render_for_prompt(known_bulletins)
```

## Replay and dedupe

- `bulletin_dedupe(bulletins)` drops duplicate proposals by stable `id`,
  preserving first-seen order.
- `bulletin_apply_decisions(bulletins, decisions)` projects the latest
  decision per `id` onto a list of bulletins, returning copies with their
  effective `status` updated. The original bulletin records are not mutated.
- `bulletin_partition(bulletins)` returns
  `{proposed, accepted, rejected, expired, superseded}` lists.

## Action boundary

`bulletin_emit` returns a `harn.profile_bulletin_emit_receipt.v1` envelope
with the EventLog id and the `proposed` bulletin payload.
`bulletin_emit_decision` returns
`harn.profile_bulletin_decision_receipt.v1`. Hosts subscribe to
`personas.bulletins.proposed` and `personas.bulletins.decisions` for review
surfaces and to fan out cloud sync once a decision is durable. Harn never
performs the persistence; the host decides what (if anything) to store and
which sync tier (`local_only`, `host_default`, `tenant`, `shared`) applies.
