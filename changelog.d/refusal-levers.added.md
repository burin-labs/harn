`code_index.move_symbol` takes `allow_visibility_widening` (`pub(super)`,
`pub(crate)`, `pub`, or `export` for TypeScript). A move that would leave a
declaration unreachable still refuses by default and now names the narrowest
fix in `required_visibility`; with the parameter it widens exactly the blocking
declarations, each to the narrowest level within the allowance, and lists them
in `widened`. A refused `ast.insert_at_anchor` (`no_match`, `ambiguous`) now
lists the containers a retry can target in `candidates`. The qualification
probe's `insert_method` arm targets traits and interfaces, and its class query
no longer matches unrelated classes.
A Rust move no longer skips the source's own import of the moved item when only a
nested module, such as `mod tests`, already imported it.
A Rust `pub(super)` or `pub(in ...)` item is no longer treated as crate-wide: a move
that leaves it out of reach of the moved code, or of the modules using the moved
item, now refuses (or widens, when allowed) instead of applying an unusable import.
`edit_insert_at_anchor` returns the retry `candidates` of a refused insert, and the
refusal's `details` names the first few.
Rust member widening proves each receiver in its lexical scope. A nested
parameter, local shadow, generic type parameter, or unrelated qualified type
cannot authorize widening a same-named member of another type.
Custom derives and conditional macro attributes cannot supply an unseen type
that authorizes widening. Built-in derives and conditional inert attributes
retain the same lexical proof.
