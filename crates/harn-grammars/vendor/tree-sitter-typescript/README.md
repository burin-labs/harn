# TypeScript grammar

These grammars are based on tree-sitter-typescript 0.23.2 (MIT). The grammar
allows generic calls containing import type queries while retaining ordinary
comparison expressions. The generated parsers use tree-sitter CLI 0.25.10 and
tree-sitter-javascript 0.23.1. `common/define-grammar.js` owns the change for
both TypeScript and TSX.

Install the pinned dependencies with `npm ci`, then run
`../node_modules/.bin/tree-sitter generate` from each dialect directory.
Remove this fork once upstream supports both import type arguments and the
comparison controls in `tests/stdlib/typescript_import_query_test.harn`.
