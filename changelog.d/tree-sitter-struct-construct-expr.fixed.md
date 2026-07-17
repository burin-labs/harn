The tree-sitter grammar now parses struct construction
(`TypeName { field: value }`) in every expression position — `return`,
ternary operands, and list/dict elements — matching the runtime parser.
Previously it was only recognized in binding initializers and call
arguments, so a passing program such as `return Basket { items: ... }`
produced a spurious `ERROR` node.
