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

## Why a reminder, not a hard router

The compass *steers* rather than *rewrites*. A reminder keeps the model
in control: it can still choose a freeform patch when a file has no
tree-sitter grammar (the structural tools degrade to `Unsupported` there
anyway), and it never silently changes the bytes a tool call would
produce. That makes the behaviour predictable and auditable — the
reminder is visible in the transcript like any other system reminder.

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

Flipping the compass on by default, together with the tool-rewrite router
that detects a freeform edit and rewrites it to the structural form, is
tracked as follow-up #2612.

## Composes with

- [Structured refactorings cookbook](./structured-refactorings.md) — the
  tools the compass points at.
- [Rename a symbol cookbook](./rename-symbol.md) — the cross-file rename
  the compass calls out by name.
- [System reminders](../system-reminders.md) — the delivery mechanism.
