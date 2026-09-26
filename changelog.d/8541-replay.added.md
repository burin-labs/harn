Decision evaluations can record and strictly replay existing tapes through
`harn llm evaluate --tape` and `harn run --evaluation-tape`. Replays retain the
original receipt separately, report zero current requests and charges, and fail
on missing, changed, or extra records. Optional `harn run --evaluation-cache`
reuses identical complete answers within the execution. Receipts distinguish
stable request identity from each invocation.
