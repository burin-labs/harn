# Completion gate (`std/agent/judge`)

A tool-using agent stops when the model says it's done. That is the model's
opinion, not a fact. The completion gate turns "done" into something you can
check: a deterministic veto built from write and verifier evidence, with an
optional bounded LLM judge on top. It is the peer of the
[governors](./governors.md) and [lanes and overlays](./agent-lanes-overlays.md)
— one concern, one module, one value you fold into the `agent_loop` options dict.

`agent_completion_gate` composes existing loop seams rather than adding a hook:
the deterministic part rides the `verify_completion` closure the loop already
consults at done-time, and the optional judge rides the capped
`verify_completion_judge` / `done_judge` seam. Harn owns the veto arithmetic;
your host supplies every domain fact (what counts as a source write, whether the
verifier is green) through callbacks.

## `agent_completion_gate`

```text
agent_completion_gate(options: CompletionGateOptions = {}) -> dict
```

Returns an options fragment you spread into your `agent_loop` options. The
fragment carries a `verify_completion` closure (the deterministic ladder) plus a
`_completion_gate` metadata row; when `judge` is set it also carries the judge
seam config.

```harn
import { agent_completion_gate } from "std/agent/judge"

const gate = agent_completion_gate({
  facts: { ctx -> {source_write_count: 1, verify: {ok: true}} },
  max_vetoes: 3,
})
log(gate._completion_gate.facts_available)
log(gate._completion_gate.max_vetoes)
```

To use it, spread the fragment into your base options:

```harn,ignore
import { agent_completion_gate } from "std/agent/judge"

agent_loop(task, system, base_opts + agent_completion_gate({
  facts: fn(ctx) { return host_completion_facts(ctx.session_id) },
  verify_command: fn() { return host_run_verify() },
  judge: true,          // optional bounded LLM judge, capped at 5 by default
}))
```

### Options

`CompletionGateOptions` splits into host-fact callbacks and plain-data policy
knobs. Every field is optional.

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `facts` | `fn(ctx) -> CompletionFacts` | — | The primary fact supplier. `ctx` is `{session_id, task, stop_reason, text, messages}`. |
| `classify_write` | `fn(path, diff?) -> WriteKind` | — | Labels one write when `facts` returns a `writes` list without counts. |
| `verify_command` | `fn() -> CompletionVerifyVerdict` | — | Runs the verifier oracle when `facts` carries no `verify` verdict. |
| `feedback_decorator` | `fn(reason, feedback, verdict) -> string` | — | Decorates a delivered veto using its stable ladder reason and structured verdict. |
| `require_source_write` | bool | `true` | Enforce the source-write evidence requirement. |
| `requires_write` | bool | per-task fact | Override the "this task needs a source change" fact. |
| `max_vetoes` | int | `3` | Per-session soft-veto budget; `0` disables. |
| `veto_combine` | `fn(verdicts) -> CompletionVerifyVerdict` | AND-of-oracles | Override how multiple verifier verdicts combine. |
| `judge` | bool / dict | off | Attach a bounded LLM judge (`true` or a judge-config dict). |
| `judge_seam` | string | `"verify_completion_judge"` | Which capped LLM seam the judge rides (`"verify_completion_judge"` or `"done_judge"`). |
| `feedback_templates` | dict | defaults | Override feedback by ladder key; repeated failures support `{attempts}` and `{findings}`. |
| `escalation_threshold` | int | `3` | Failed-after-write streak required to recommend escalation. |
| `escalation_target` | string | — | Host routing channel copied onto an escalated verdict and event. |

The fact types:

- `CompletionFacts` — `{consecutive_failed_after_write?,
  source_write_count?, cosmetic_write_count?, writes?, verify?,
  verification_failures_converging?, requires_write?}`. Supply counts directly,
  or a `writes` list of `CompletionWriteFact` for the gate to classify.
  `verify` is one `CompletionVerifyVerdict` or a list of them. When the streak
  is present, a red verifier uses the streak-aware reasons below; a converging
  diagnostic set consumes churn credit and suppresses escalation for that
  decision.
- `CompletionWriteFact` — `{path?, diff?, kind?}`. `kind` is a `WriteKind`
  string; `"source"` and `"cosmetic"` are load-bearing, anything else is treated
  as non-source.
- `CompletionVerifyVerdict` — `{ok?, findings?}`. `findings` is optional
  red-detail text.

## The deterministic ladder

The gate never keys on a done-sentinel string. It decides purely from write and
verifier facts, first match wins:

| Reason | Result | Condition |
| --- | --- | --- |
| `no_source_write` | veto (soft) | task requires a source change, but only cosmetic / zero source writes so far |
| `verification_after_write_red` | veto (**strict**) | a source write with a red verifier and no streak fact — backward-compatible path |
| `failed_verification` | veto (**strict**) | streak-aware red verifier below the threshold, or diagnostic churn is still converging |
| `repeated_verification_failures` | veto (**strict**) + escalation | non-converging red verifier at or above `escalation_threshold` |
| `verified_after_write` / `verified` | allow | verifier is green |
| `missing_verification` | veto (**strict**) | source written, verifier configured, not yet run — the budget never releases it |
| `no_workspace_write` | allow | task does not require a source change |
| `veto_budget_exhausted` | allow | a soft veto after `max_vetoes`, converted to an attributable end |

Only **source** writes count as progress toward done. A cosmetic final write (a
comment, a `.md` typo) is not evidence: it can't flip an already-green run back
to unverified, and a run that wrote only cosmetics can't claim done. A **strict**
veto (a source write with a red or missing verifier) is never released by the
veto budget — the run does not get to declare victory on a failing build.

Each deterministic decision emits a `judge_decision` event with
`trigger: "verify_completion"`, `confirm`, and the stable `reason` above. When
the budget converts a soft veto into an allow, the event carries
`reason: "veto_budget_exhausted"` plus `converted_from` with the original class.
Streak escalation decisions also carry `escalation_recommended` and, when
configured, `escalation_target`. `agent_turn(...).judge_decisions` preserves
the same fields.

`feedback_decorator` runs only after the ladder and veto budget resolve a veto
that will actually be delivered. It receives `(reason, feedback, verdict)`;
the verdict includes any escalation fields, so a host can attach
language-specific repair cues without parsing the default prose.

### Degraded mode

With **no** host-fact callbacks (`facts`, `verify_command`, and `classify_write`
all absent), the deterministic gate has nothing to assert, so it abstains — it
allows, and marks `_completion_gate.facts_available = false` with a verdict
reason of `facts_unavailable`. It never fabricates a pass. Any configured LLM
judge still runs, so this is judge-only mode, not no-op mode.

## The optional bounded judge

Set `judge` to add an LLM check on top of the deterministic ladder. `judge:
true` uses defaults; a dict passes through provider, model, system prompt, and
cap overrides. The judge rides the capped `verify_completion_judge` seam by
default (or `done_judge` via `judge_seam`), so it inherits that seam's
per-session veto cap — 5 by default. Past the cap the judge stops firing and the
loop ends with status `verify_capped`; set `max_invocations: 0` to disable the
cap. Two helpers surface the resolved cap for run records:

```text
agent_verify_completion_judge_cap(judge_cfg) -> int | nil
agent_done_judge_cap(judge_cfg) -> int | nil
```

Both return `nil` when the cap is disabled.

## See also

- [Agent guardrails](./agent-guardrails.md) — the input-side bookend that can
  stop before the first main model turn.
- [Agent loops: completion gate](../llm/agent_loop.md#completion-gate-agent_completion_gate)
  — the same gate in the context of the loop's other completion seams
  (`verify_completion`, `verify_completion_judge`, `done_judge`).
- [Host-supplied facts](./fact-intake-seams.md) — the broader
  Harn-owns-mechanism / host-owns-facts pattern this gate follows.
- [Agent governors and detectors](./governors.md) — the budget and stall side of
  the same placement contract.
- [The expressiveness spectrum](../concepts/expressiveness-spectrum.md#level-4-tune-the-loop-by-composing-building-blocks)
  — where the completion gate sits when you compose loop control by hand.
