Sorting (`sort`, `sorted`, `sort_by`, `sorted_by`) no longer panics on lists that
mix NaN or values of different types: the sort comparator is now a total order
that places numbers first, then NaN, then other types grouped by kind.
`pascal_to_camel("HTTPServer")` returns `httpServer` instead of `hTTPServer`.
`iter.sum()` shares `list.sum()` and so sums decimals. `round(x, -n)` returns
exact power-of-ten buckets for floats (`round(123456.0, -5)` is `100000.0`) and
no longer zeroes ints of 5e18 and beyond at `-19`. Set `union`, `intersection`,
`difference`, and `symmetric_difference` methods throw on a non-set argument
like their `set_*` builtins. Local JSON repair of structured LLM output keeps
non-ASCII text intact instead of mojibaking it. `zip_create` refuses `..` and
absolute entry paths, matching `tar_create`. `harn fmt` keeps the parentheses
around a `try*` operand, so `(try* fetch()).body` no longer reformats into
`try* fetch().body`.
