# Fixer Persona

Fixer consumes `invariant.blocked_with_remediation` events from Harn Flow.
Remediation suggestions are inert until Fixer materializes them as freshly
signed atoms and proposes a follow-up slice that includes both the original
blocked slice atoms and the remediation atoms.

The v0 workflow is intentionally declarative. Runtime hosts provide the actual
event envelope, signing keys, and approval UX while `harn-vm` owns deterministic
follow-up slice construction.

Validate locally:

```bash
harn persona --manifest personas/fixer/harn.toml inspect fixer --json
harn check personas/fixer/manifest.harn
```
