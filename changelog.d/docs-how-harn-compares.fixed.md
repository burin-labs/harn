**`npm run dev` renders the docs site again.** The client entry hydrated
whenever the root element had any child node, and in development the unreplaced
`<!--app-html-->` placeholder is itself a child, so React hydrated against
markup that was not there and left a blank page. It now tests for an element.

**`retry` is documented correctly.** It retries on any error rather than
classifying them, and the last error propagates when every attempt fails; the
reference previously said the block returns `nil`. The retry example now
explains why that generality is what makes it work, and points at
`harness.llm.with_rate_limit` for the error-aware alternative.

**Docs fact-check pass.** `why-harn.md` and `builtins.md` both named a stale
short list of built-in LLM providers — they omitted Gemini, Groq, and DeepSeek
while naming HuggingFace, which is supported but not featured. Both now give the
real count (44) and link the capability matrix. The AWS Strands link in
`sota-comparison.md` 404'd after Strands restructured its docs. Two sandbox
ratings were corrected against primary sources: Temporal's Python workflow
sandbox is a determinism boundary its own docs call not completely isolated, and
Cursor's cloud agents run in per-agent Firecracker microVMs, which the previous
note understated. Illustrative environment variables in examples no longer wear
the `HARN_` prefix the runtime reserves for its own.
