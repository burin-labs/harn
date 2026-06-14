- **Deeply nested values can no longer abort the process with a native stack
  overflow.** A script could build a value nested far deeper than the call
  stack tolerates (`x = [x]` in a loop adds no VM frames, so `max_vm_frames`
  never fired), then crash the whole host — `SIGABRT`, bypassing every runtime
  limit — simply by comparing (`==`), printing, `json_stringify`-ing, sorting,
  hashing (set/dict de-dup), or even just dropping it. The recursive value
  walks (equality, ordering, structural hashing, display, JSON) now grow the
  native stack on demand (the approach serde/rustc/syn take, via `stacker`), so
  deep-but-finite data completes instead of crashing; value teardown across the
  VM's slot/scope lifecycle is now iterative; and the `serde`-backed
  pretty-JSON / YAML encoders reject values past `max_value_depth` (1024) with a
  catchable error rather than overflowing. Mirrors `serde_json`'s
  deserialization recursion limit and CPython's recursion guards in its C-level
  `json`/comparison paths.
