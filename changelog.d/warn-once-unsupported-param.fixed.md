Unsupported sampling-param warnings (`"top_k" is not supported by provider …,
ignoring` and the seed/penalty/cache siblings) now emit once per
`(param, provider, model)` instead of on every LLM call, so they no longer
flood agent and eval logs.
