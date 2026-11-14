Rust embedders constructing `ToolContractViolation` must initialize its new
`wire_failure` field, or use `..` when matching it. The optional value retains
the wire failure code, declaration source and structural path without rejected
application values.
