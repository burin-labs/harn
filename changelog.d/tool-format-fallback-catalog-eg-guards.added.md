- **Runtime tool_format fallback and equivalence-group catalog guards.** A
  native-tool-format request whose failure fingerprint says the provider's
  server-side tool-call parser choked (the Ollama 500 / EOF leak, or any serving
  stack that 500s/EOFs on the native assumption) now degrades once to the text
  channel and retries there instead of parse-looping or hard-failing — keyed on
  the failure signature, never a model name. The provider catalog also gains two
  build-time invariants: every active row in an `equivalence_group` must declare
  the same `tier` (a capability of the logical model, not of who hosts it), and a
  local-runtime row may not carry `strengths` beyond its group's conservative
  baseline (so a local route cannot inherit a cloud peer's decoration and read as
  already-capable). Both invariants pass on the shipping catalog and only fail the
  build if a future change reintroduces the divergence.
