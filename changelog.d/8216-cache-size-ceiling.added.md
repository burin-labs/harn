The shared development cache sweep can now be bounded by size. Its rules
answered how many entries a root keeps and how long one may sit idle, but
never how large the root may grow, so a busy fleet that touched every entry
inside the idle window kept all of them and the sweep reported a healthy pass
at any size.

`HARN_TARGET_GC_MAX_BYTES` sets a per-root ceiling. Once the entries a run
kept exceed it, the coldest are retired until the root fits. It runs after
every other rule, so a live process, the caller's own entry, and an explicitly
named entry all outrank it. It is off by default because enforcing it costs a
full measurement of every kept entry, which is not a price each development
setup should pay; enable it on the periodic cleanup path. An entry that cannot
be measured is reported rather than counted as free space.
