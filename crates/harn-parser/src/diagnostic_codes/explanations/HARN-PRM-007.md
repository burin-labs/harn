# HARN-PRM-007 — prompt or template target cannot be found

**Category:** Prompt template (PRM)  
**Variant:** `Code::PromptTargetMissing` (prompt target missing)

## What it means

A prompt template (`.harn.prompt` / `.prompt`) failed validation, either because
the template body is malformed or because it references the model surface in a
way that violates Harn's prompt safety rules.

Specifically: prompt or template target cannot be found.

## How to fix

- Fix the template syntax (`{{ }}`, `{% %}` are the only structural delimiters).
- Replace identity-based branching with capability flags from the LLM call options.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.
