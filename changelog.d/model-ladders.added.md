- **Model ladders.** `llm_call` (and the `llm_call_structured*` variants) gain
  a first-class `models:` option — an ordered fallback ladder of
  `{model, provider?, options?}` steps (plain `["model-a", "model-b"]` strings
  are sugar) — plus `ladder: "<name>"` to resolve a named `[model_ladders.<name>]`
  ladder from the catalog. A ladder lowers onto the existing routing chain, so
  it advances to the next step **only** on transport-class failures
  (connection/timeout/429/5xx/throttled-empty, via the same failover
  classifier) and never on schema-validation or 4xx policy errors. Schema
  retries re-ask the same step's model. Each advance emits an
  `llm_models_advance` trace event (`agent_trace()`), and the winning model is
  surfaced on the existing `routing` result block. `models:`/`ladder:` cannot be
  combined with each other or with an explicit `model:`/`provider:` pin.
