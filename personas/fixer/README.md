# Fixer persona

Fixer consumes `invariant.blocked_with_remediation` events from Harn Flow.
Remediation suggestions are inert until Fixer materializes them as freshly
signed atoms and proposes a follow-up slice that includes both the original
blocked slice atoms and the remediation atoms.

The v0 workflow is intentionally declarative. Runtime hosts provide the actual
event envelope, signing keys, and approval UX while `harn-vm` owns deterministic
follow-up slice construction.

## Edit strategy

Fixer's mutation half routes through the AST-precise primitives in
[`std/edit`](../../docs/src/stdlib/edit.md): `edit_apply_node` for node
replacement, `edit_insert_at_anchor` for sibling/child inserts,
`edit_rename_symbol` for cross-file identifier renames, `edit_dry_run` to
preview a multi-op plan, and `edit_safe_text_patch` as the text fallback
when the language has no tree-sitter grammar or the change is purely
textual. `lib/remediation_plan.harn` maps each incoming remediation atom
to the narrowest primitive that fits its shape; the decision tree and
the agent-loop `system_reminder` snippet that propagates the same
preference live in
[Precise edits with AST tools](../../docs/src/cookbook.md#precise-edits-with-ast-tools).

Validate locally:

```bash
harn persona --manifest personas/fixer/harn.toml inspect fixer --json
harn check personas/fixer/manifest.harn
harn test personas/fixer/tests/
```
