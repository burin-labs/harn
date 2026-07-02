Corpus-driven text tool-call parser tolerance, grounded in 526 mined eval runs: back-to-back
`</tool_call><tool_call>` blocks on one line no longer shred into stray-text violations; `<invoke>` markup
tolerates extra `<parameter ...>` attributes (`string="true"`) instead of misdiagnosing complete calls as
truncated; `<function_calls>` wrapper tags are swallowed silently; an unclosed terminal
`<user_response>`/`<assistant_prose>` is accepted as the block body instead of killing the final answer;
compat aliases (`replace_range`, `bash`, ...) now fold in the bare TEXT channel like they already did
natively; `<|...|>` provider tokens are stripped from unresolvable tool names; and dispatch coerces
`"True"`→bool and JSON-array strings→list on unambiguous schema expectations. Each observed failing
emission shape is pinned as a conformance fixture in `tools/tests/corpus_conformance.rs`.
