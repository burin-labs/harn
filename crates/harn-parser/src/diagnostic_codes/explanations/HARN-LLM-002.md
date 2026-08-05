# HARN-LLM-002 — LLM option key is deprecated

## How to fix

- Pass a `schema:` option that validates the model output, with `schema_retries:` as appropriate.
- Drop or rename deprecated options to the names listed in the LLM call quickref.
- Pick capability flags (`tool_calling: true`, etc.) instead of branching on `provider:` identity.
