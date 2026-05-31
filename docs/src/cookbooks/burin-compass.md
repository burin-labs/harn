# Burin compass: steer toward AST edits

Harn ships a moat-quality set of **AST-precise edit primitives** — see
the [structured refactorings cookbook](./structured-refactorings.md).
They only pay off, though, if the agent loop *reaches for them first*.
Left to its own devices a model takes the path of least resistance — a
freeform `str_replace` / line patch — which is exactly the brittle,
string-collision failure mode the structural tools exist to eliminate.

The **burin compass** inverts that default. It is a built-in
[system-reminder](../system-reminders.md) provider, `compass_ast_edits`,
that injects a standing reminder at session start (and on resume):

> When editing source files, prefer the AST-precise edit tools over
> freeform text edits: `edit_apply_node` / `edit_insert_at_anchor` for
> node-level changes, `edit_rename_symbol` for safe cross-file renames,
> and the structured refactors (`edit_extract_function`,
> `edit_change_signature`, `edit_inline`, `edit_move_decl`, …) for
> compound changes. Preview any plan with `edit_dry_run` before
> committing. Reach for `edit_safe_text_patch` only when the language has
> no grammar support.

The compass ships **opt-in**. Unlike the other canonical providers —
which are conditional and stay silent until they have something to say
(project facts, a workspace anchor, token pressure) — a steer toward code
edits is only wanted in code-editing sessions, not in every sub-agent or
one-shot loop. So coding-agent personas and configs turn it on
explicitly; once enabled it reaches every agent surface that runs the
Harn agent loop — TUI, IDE, cloud-supervised. The reminder is marked
`preserve_on_compact`, so the guidance survives a context compaction and
keeps steering through a long session.

## The active routing layer

The reminder is the *steer*; the **tool-rewrite router** is the active
half of the compass (#2612). It is a per-tool-call hook in the agent loop
that observes a freeform edit *before it runs* and acts on it. It sits at
the single chokepoint every agent surface funnels through — after
permission and pre-tool hooks, before the tool is dispatched — so it
reaches the TUI, the IDE, and cloud-supervised loops with no per-surface
wiring.

It recognises a freeform / whole-file edit on a *parseable* source file:

- a `str_replace`-shape call (`{path, old_text, new_text}` or a `hunks`
  array),
- a whole-file `write_file` / `create_file`,
- a single-hunk edit on a rename-capable language that reads like a
  symbol rename.

For anything else — a structural call, a file with no tree-sitter grammar,
an arg shape it doesn't understand — the router is inert and the call
dispatches unchanged. It is **conservative by construction**: it never
touches a call it cannot reason about.

It runs in one of two modes:

- **`suggest`** (the default) — advisory. The router injects a
  one-turn system reminder naming the structural primitive the edit maps
  to (`edit_apply_node` for a node, `edit_rename_symbol` for a rename, or
  the hash-guarded `edit_safe_text_patch`), then dispatches the original
  call unchanged. The model stays in control; nothing is rewritten.
- **`rewrite`** — silent substitution, but only when the structural form
  is *provably equivalent* to the freeform call. The one substitution the
  router can prove without reading the file is a raw text replace →
  `edit_safe_text_patch`: identical `old_text → new_text` matcher, but with
  a stale-base hash guard and staged-fs atomicity. A rename or a whole-file
  write is **not** byte-equivalent (a project-wide rename touches other
  files; a whole-file write rewrites untouched bytes), so the router falls
  back to a suggestion rather than guess.

### Observability

Every decision increments a `harn.compass.*` counter via the standard
`counter(...)` instrument, tagged with `harn.compass.persona`,
`harn.compass.tool` (the freeform tool), and `harn.compass.target` (the
structural tool):

- `harn.compass.suggested` — an advisory routing decision fired.
- `harn.compass.rewritten` — a call was silently substituted.
- `harn.compass.fell_back` — `rewrite` mode considered a substitution but
  could not prove equivalence, so the original freeform call ran.

The router also emits a live `compass_routing_decision` agent event before
dispatch. ACP surfaces it on `_harn/agentEvent` with `toolCallId`, `mode`,
`action` (`suggested`, `rewritten`, or `fell_back`), `persona`,
`originalTool`, `routedTool`, `targetTool`, and `path` when the edit call
named a file. The event carries routing metadata only; it does not include
old or new file contents.

These surface in eval dashboards as the agent-loop edit-reliability signal.

## Why a reminder by default, not a hard rewrite

In `suggest` mode the compass *steers* rather than *rewrites*. A reminder
keeps the model in control: it can still choose a freeform patch when a
file has no tree-sitter grammar (the structural tools degrade to
`Unsupported` there anyway), and it never silently changes the bytes a
tool call would produce. That makes the behaviour predictable and
auditable — the reminder is visible in the transcript like any other
system reminder. `rewrite` mode is opt-in for exactly this reason: it only
ever substitutes a provably-equivalent call.

## Turning it on

The compass is a normal reminder provider, so the standard controls
apply:

- **Enable it** for a session or persona via the reminder config:
  `reminders.providers.compass_ast_edits = true`. It is registered as a
  canonical provider but ships `default_enabled: false`, so this opt-in is
  what activates the steer.
- **Inspect it** alongside the other canonical providers —
  `compass_ast_edits` appears in the provider metadata listing with the
  summary "Steer the agent toward AST edit primitives over freeform text
  edits."

## Configuring the router (escape hatches)

The router is on by default in `suggest` mode for every agent loop. It is
controlled by the `compass` option passed alongside the agent-loop tools
and is fully gated:

- `compass: false` — turn the router off entirely. Freeform edits dispatch
  with no observation, no reminder, no counter.
- `compass: {enabled: false}` or `compass: {mode: "off"}` — equivalent
  off switch in dict form.
- `compass: {mode: "suggest"}` — the default; advisory reminders only.
- `compass: {mode: "rewrite"}` — silent substitution of
  provably-equivalent calls, with a fall-back-to-suggest safety net.
- `compass: {prefer: ["edit_apply_node", ...]}` — a persona's ordering
  hint. The router consumes the `edit_strategy.prefer` list a persona
  already declares (e.g. `personas/fixer/manifest.harn`), so a persona
  that prefers node-level editing nudges a plain hunk edit toward
  `edit_apply_node` in its suggestion. When no `compass.prefer` override is
  set the router reads `edit_strategy.prefer` directly.

The escape hatch the *model* always has: ignore the suggestion. A
`suggest` reminder never blocks or alters the call, and even a `rewrite`
only ever swaps in a behaviourally identical patch — so a deliberate
freeform edit on an unsupported file, or a one-off raw write, is always
available.

## Composes with

- [Structured refactorings cookbook](./structured-refactorings.md) — the
  tools the compass points at.
- [Rename a symbol cookbook](./rename-symbol.md) — the cross-file rename
  the compass calls out by name.
- [System reminders](../system-reminders.md) — the delivery mechanism.
