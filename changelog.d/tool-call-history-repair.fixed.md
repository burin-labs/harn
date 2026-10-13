Every provider now receives the same repaired tool-call history. A tool result
whose call was compacted away is sent as an observation, and runtime feedback
injected between a call and its result moves after the result, on every route.
Previously OpenAI Chat Completions refused interleaved feedback, and OpenAI
Responses and Gemini refused an orphaned result, while Anthropic accepted both.
Calls left unanswered by a stopped turn keep their existing typed repair result.
