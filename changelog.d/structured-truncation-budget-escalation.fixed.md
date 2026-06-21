A structured/`llm_call`-with-schema retry that failed because the response was
truncated by `max_tokens` mid-JSON now doubles the output-token budget (capped
at 32,768) before the retry instead of replaying the same under-budget call.
Reasoning models (gpt-oss/Harmony, DeepSeek-R, o-series) bill their analysis
channel against the same output budget while it stays invisible in the parsed
text, so a budget that comfortably fits a non-reasoning model's JSON gets
consumed entirely by reasoning — truncating the visible JSON to empty and, once
schema-retry slots are exhausted, returning a DEAD `length_truncation` envelope
(an empty judge verdict that silently falls through to the deterministic
grader). The escalation is provider-agnostic, keyed off the existing
`is_length_truncation` truncation marker.
