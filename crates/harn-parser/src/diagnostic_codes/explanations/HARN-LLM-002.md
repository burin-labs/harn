# HARN-LLM-002 — LLM option key is deprecated

**Category:** LLM call (LLM)  
**Variant:** `Code::DeprecatedLlmOption` (deprecated llm option)

## What it means

The `llm_call(...)` invocation here violates the schema Harn enforces for LLM
calls. Schema-validated, provider-portable LLM calls are a load-bearing Harn
contract; drift in the options table is rejected at check time.

Specifically: LLM option key is deprecated.

## How to fix

- Pass a `schema:` option that validates the model output, with `schema_retries:` as appropriate.
- Drop or rename deprecated options to the names listed in the LLM call quickref.
- Pick capability flags (`tool_calling: true`, etc.) instead of branching on `provider:` identity.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.
