- Cross-cutting correctness + performance sweep:
  - `fs_snapshot`: a snapshot whose captured bytes exceed the whole session byte cap is no longer evicted by the very
    call that created it — `enforce_byte_cap` now protects the snapshot currently being written, fixing a panic in
    `snapshot()` (it re-fetched the just-evicted snapshot) and silent loss of rollback for an in-flight write.
  - `fs_snapshot`: `atomic_write` no longer leaks its temp file when both the rename and the
    remove-then-rename retry fail.
  - Package registry: `harn add <pkg>` with no version constraint now resolves the highest **stable** release rather
    than letting an `x.y.z-rc.1` prerelease shadow it (matching cargo/npm); packages that have only ever published
    prereleases still resolve to the highest prerelease.
  - `harn scan` regex rules: row/column for each match is now computed with a single forward cursor instead of
    rescanning the document from byte 0 per match — the matcher was O(matches × file length) on files with many hits.
  - Deterministic `search` tool: the compiled `RegexMatcher` is now borrowed per file instead of deep-cloned, so a
    repo-wide scan no longer re-copies the compiled regex program once per file.
