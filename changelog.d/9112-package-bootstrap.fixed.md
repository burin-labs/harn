Package verification generates missing CLI bytecode before certifying its Harn
binary, and uses one feature graph for Harn, its freshness checker, and the AOT
generator. Repeated preparation preserves provenance and honors no-build mode.
