Context-eval v2 reports use `null` for `aggregate.total_known_cost_usd` when no cost or lower bound was observed.
Consumers must handle `null` separately from a measured zero. Rust callers must handle
`ContextEvalAggregateV2::total_known_cost_usd` as `Option<f64>`; Harn callers must handle it as `float?`.
