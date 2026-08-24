Corrected the documented behaviour of `retry`. It retries on any error rather
than classifying them, and the last error propagates when every attempt fails;
the reference previously said the block returns `nil`. Added an explainer to
the retry example and a pointer to `harness.llm.with_rate_limit` for the
error-aware alternative.
