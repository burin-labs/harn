Path globs (`glob_match`, path-keyed tool argument constraints, invariant and
skill globs) now match through `globset` with a literal separator instead of a
hand-written matcher. `*` and `?` still never cross `/`. Matching is byte-wise
as in git's wildmatch, so over a non-ASCII name `?` consumes one UTF-8 byte. A
negated class such as `[!x]` can match `/`, and a pattern that does not parse,
such as an unclosed `[`, matches only that exact path.
