//! Source-aware lint rules that operate on raw source plus the AST,
//! rather than on the stateful [`Linter`][crate::linter::Linter] walk.
//! Each rule lives in its own submodule so the dispatch list in
//! [`Linter::lint_program`][crate::linter::Linter::lint_program] stays
//! legible.

pub(crate) mod blank_lines;
pub(crate) mod file_header;
pub(crate) mod import_order;
pub(crate) mod trailing_comma;
