- **`make check-docs-symbols` fails documentation that still names a removed
  runtime API.** `check-docs-snippets` type-checks fenced Harn blocks, so
  stale API names survived in the two surfaces nothing read: ordinary prose
  and the bodies of blocks tagged `harn,ignore`. The new gate reads both,
  reports the file and line, and names the replacement. The old-to-new
  mapping comes from the compiler's own harness-migration table rather than a
  list in this repository, so it cannot go stale: the checker hands every
  call-shaped identifier the docs mention to one `harn check` and keeps the
  answers whose suggested replacement is a typed `harness.*` path.
  `scripts/docs-removed-symbols-allowlist.txt` holds the 312 pre-existing
  references plus 13 reviewed exemptions, and the gate fails when one of its
  entries stops applying, so it can only shrink.
