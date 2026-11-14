Migration: Rust embedders constructing `ToolContractViolation` must initialize its new
`wire_failure` field, or use `..` when matching it. The optional value retains
the wire failure code, declaration source and structural path without rejected
application values.

Typed tool clients should follow [Migrate typed tool clients](https://harnlang.com/migrate-typed-tools.html)
for the corrected public wire representations.
