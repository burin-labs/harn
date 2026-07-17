# {{persona_title}}

Generated from the `{{template_kind}}` persona template.

## Validate

```bash
harn persona doctor {{persona_name}}
```

## Run The Smoke Test

```bash
harn test tests/{{persona_name}}_smoke.harn
```

## Package Layout

- `harn.toml` declares the durable persona manifest and authority defaults.
- `src/{{persona_name}}.harn` contains the persona and typed step DAG.
- `prompts/system.harn.prompt` contains model-facing instructions.
- `fixtures/`, `tests/`, and `evals/` contain deterministic validation assets.
