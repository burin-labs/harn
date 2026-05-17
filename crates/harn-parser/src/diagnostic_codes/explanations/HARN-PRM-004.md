# HARN-PRM-004 — prompt template branches on provider identity

**Category:** Prompt template (PRM)  
**Variant:** `Code::PromptProviderIdentityBranch` (prompt provider identity
branch)

## What it means

A prompt template (`.harn.prompt` / `.prompt`) failed validation, either because
the template body is malformed or because it references the model surface in a
way that violates Harn's prompt safety rules.

Specifically: prompt template branches on provider identity.

## How to fix

- Fix the template syntax (`{{ }}`, `{% %}` are the only structural delimiters).
- Replace identity-based branching with capability flags from the LLM call options.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.
