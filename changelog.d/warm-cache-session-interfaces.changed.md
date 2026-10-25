`harn precompile --warm-cache` now proves each imported module's interface
record once per run instead of once for every entry that reaches it, and type
checks each entry against the module graph it already built. On a large
multi-module tree the warm-cache run finishes in about half the time.
