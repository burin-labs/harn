**LLM caller middleware**: `default_llm_caller` (`std/llm/handlers`) now attaches
the underlying `error` on the `budget_exhausted` envelope branch, matching
`safe_call` (`std/llm/safe`); both share one `__wrap_llm_result` wrapper so the
budget context is no longer dropped on the handlers path. The default retry
predicate now also treats `context_overflow` as an alias of
`context_window_exceeded` (never retried), and the `with_retry` docs list the
full never-retry set.
