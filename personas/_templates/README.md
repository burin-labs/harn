# Persona templates

These packages mirror the built-in `harn persona new --template <kind>` layouts.
They are intentionally Harn-first: each template centers an `@persona` function,
typed `@step` metadata, prompt assets, a fixture pair, and a smoke test.

The CLI applies the same placeholder vocabulary when scaffolding:

- `{{persona_name}}`
- `{{persona_ident}}`
- `{{persona_title}}`
- `{{template_kind}}`
