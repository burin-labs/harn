# HARN-CAP-301 — child agent effect set exceeds the parent's declared effects

**Category:** Host capability (CAP)
**Variant:** `Code::EffectInheritanceViolation` (effect inheritance violation)

## What it means

A spawned child agent requests typed side-effects (`harness.net.*`,
`harness.fs.*`, `llm_call`, tool dispatch, ...) that are not part of the
parent agent's declared effect set. The dispatcher (and `harn check`'s
static analyzer) enforce that a child's effect set must be a subset of
the parent's so an over-delegated child can never escape its parent's
trust boundary.

Specifically: at least one effect on the child handoff is not covered by
any effect declared on the parent. Coverage is structural:

- **Kind** must match by family (`stdio`, `fs`, `net`, `llm`, `tool`,
  `hostcall`, `persona`, `spawn`).
- **Scope** must be at least as permissive on the parent
  (`read`/`observe` ≤ `write` ≤ `mutate`).
- **Resource**, when declared on the parent, must match the child's
  exactly. A parent with no `resource` covers any child resource of the
  same kind/scope family.

This diagnostic surfaces from two paths and they are intentionally
identical:

- **Static**: `harn check` derives parent and child effect sets via the
  same capability analysis that backs `harn graph --json` and emits
  `HARN-CAP-301` when a child statically out-grants its parent.
- **Runtime**: at spawn time the dispatcher folds the parent's
  `current_execution_policy()` into an effect set, compares it against
  the child's computed effects, and refuses the spawn with a typed
  `EffectInheritanceViolation` deny event. The event payload carries the
  same `HARN-CAP-301` code and `policy/narrow-child-effects` repair id.

## How to fix

The repair id is `policy/narrow-child-effects`. Two shapes are
typically valid:

1. **Narrow the child** — remove the over-granted calls from the child
   pipeline body, or guard them behind a capability the parent already
   declares. This is the safe default; the child stops asking for what
   it does not need.
2. **Widen the parent** — declare the missing effect on the parent
   pipeline (e.g. add a real `harness.net.get(...)` call in the parent's
   entrypoint, or extend the parent's `policy.capabilities` map). Only
   apply this when widening the parent's trust surface is itself an
   intentional design change — it touches a public surface.

Both shapes are marked `safety: surface-changing`, so `harn fix --apply
--safety surface-changing` will dispatch the chosen repair.

## Stability

This code is stable. Its identifier, category, and meaning will not
change without a deprecation cycle. Cross-language tooling and IDE
integrations can dispatch on it directly. The matched
`EffectInheritanceViolation` runtime payload's `_type` discriminator
(`effect_inheritance_violation`) is part of the same stable contract.
