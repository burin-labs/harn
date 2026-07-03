# Verification

`std/verification` is the Harn-owned home for deterministic verification
facts. It keeps reusable verification substrate in Harn rather than in a
particular host product: hosts supply file bytes and code-index state, while
Harn policies consume explicit facts.

## File-Hash Snapshots

`verification_file_hash_snapshot(paths)` captures current on-disk hashes for a
batch of workspace paths under one code-index sequence binding:

```harn
import { verification_file_hash_snapshot } from "std/verification"

pipeline default() {
  let _ = hostlib_code_index_rebuild({root: "."})
  let snap = verification_file_hash_snapshot(["src/main.zig", "build.zig"])
  let verdict = verification_diagnostic_classify(
    {rung: "R2", rowId: "zig/file", at: snap.captured_at_ms, snapshot: snap.snapshot},
    snap.snapshot,
  )
  return verdict.status
}
```

The returned shape includes:

- `seq`: the current code-index version-log sequence at capture time.
- `snapshot`: a direct path-to-hash map accepted by
  `verification_diagnostic_classify`.
- `missing`: paths whose current bytes could not be read.
- `files`: per-path metadata including readability, index membership,
  current hash, hash source (`indexed`, `disk`, or `missing`), indexed hash,
  mtime, size, and last edit sequence.

The hash algorithm is the same decimal FNV-1a 64-bit value used by
`hostlib_code_index_file_hash` and `hostlib_code_index_changes_since`.
Unknown-but-readable files are still included in `snapshot` with `known =
false`, which lets a diagnostic bind to a newly created file before the next
full code-index rebuild.
