Prompt source maps retain the real source line and column for literal text,
raw blocks, and bare interpolations, including text after whitespace trimming.
Each span and its include ancestry carry the source revision so debuggers and
recording readers can distinguish the recorded source from later edits.
Identical rendered text from different source maps retains distinct recording
identities.

Migration: Rust consumers must replace `Node::Text(text)` with
`Node::Text { text, line, col }`, supply `line` and `col` when constructing
`Node::LegacyBareInterp`, and populate `PromptSourceSpan::template_revision`
with the source content hash. Matchers that do not inspect positions can use
`..`. Coordinates are one-based; prompt text syntax is unchanged.
