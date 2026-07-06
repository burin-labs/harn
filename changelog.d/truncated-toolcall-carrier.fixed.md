A streamed tool call whose native arguments are cut off mid-stream (a
`{"__parse_error": "..."}` carrier) is now named as a truncated or malformed
call and coached to re-issue smaller, instead of the misdiagnosing "missing
required parameter: path" that sent models (observed on llama.cpp qwen3.6-35b)
into a 20+ call re-try spin with no visible reply.
