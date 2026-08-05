Tree-sitter crates now arrive as their own Dependabot group instead of riding
in the routine `cargo-minor-patch` batch. A grammar bump is not a routine
dependency bump — it changes how source parses, and it is answered by the
`harn-hostlib` parser-agreement corpus rather than by a version number.

In the catch-all it also held unrelated work hostage. Twice a single
`tree-sitter-swift` bump blocked seventeen other crates from merging, because
the group could not land until the grammar question was settled. Its own pull
request gets its own review and its own revert, which is why the `cranelift`,
`wasmtime`, and `opentelemetry` families are already carved out the same way.

The membership is checked rather than conventional: `scripts/check_dependabot_groups.harn`
now carries the family, so dropping the exclusion that keeps grammars out of the
catch-all fails the gate with the exact crates that became ambiguous.
