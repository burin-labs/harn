# HARN-PRM-007 — prompt or template target cannot be found

## What it means

A prompt template (`.harn.prompt` / `.prompt`) failed validation, either because
the template body is malformed or because it references the model surface in a
way that violates Harn's prompt safety rules.

## How to fix

- Fix the template syntax (`{{ }}`, `{% %}` are the only structural delimiters).
- Replace identity-based branching with capability flags from the LLM call options.
