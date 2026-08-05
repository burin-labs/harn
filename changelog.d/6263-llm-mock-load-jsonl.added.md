`HarnessLlm` now exposes `mock_load_jsonl`, so a pipeline can install a
versioned JSONL mock fixture without a host round-trip.

The builtin behind it already described itself as a capability — it takes the
fixture TEXT precisely so every host shares one parser without granting ambient
reads to scripts — but it was only reachable as `runtime_internal`. Source that
needed it had no path at all: the bare call was refused as not-callable source
API, and `harness.llm.mock_load_jsonl` type-checked and then threw
`HarnessLlm has no method` at the call.
