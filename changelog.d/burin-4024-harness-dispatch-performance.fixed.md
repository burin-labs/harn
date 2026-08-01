Typed Harness and builtin calls now resolve contracts through indexed manifest
projections instead of allocating and scanning the full registry on every
runtime call. Effect metadata travels with numeric dispatch entries, and typed
methods avoid owned dispatch keys. The syntax-sensitive repository guard also
narrows files and lines before regex evaluation, including when restricted
runtimes cannot use its git fast path, instead of interpreting every line with
a quadratic length loop. Together these changes restore fast audit scripts and
the proven four-way CI topology.
