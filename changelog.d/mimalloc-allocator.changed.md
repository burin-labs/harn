- **Allocator.** The `harn` binary now links [mimalloc](https://github.com/microsoft/mimalloc)
  as its global allocator by default, lowering per-allocation latency and
  fragmentation on the runtime's allocation-heavy copy-on-write workload.
  Opt out with `--no-default-features` (or omit the new default-on `mimalloc`
  Cargo feature) to fall back to the system allocator.
