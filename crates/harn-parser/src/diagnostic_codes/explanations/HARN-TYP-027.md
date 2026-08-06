# HARN-TYP-027 — constant tuple index is outside the fixed arity

A `tuple<T0, ...>` has a statically known number of positions. A constant
subscript must name one of those positions, including Harn's negative-index
spelling (`-1` is the final position).

Use an in-bounds index, destructure the tuple, or widen it to a `list<T>` when
the collection is intentionally variable-length.
