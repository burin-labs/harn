- **`scripts/release_gate.sh audit` is faster and surfaces failures up front.**
  `sync_language_spec` (the `spec/HARN_SPEC.md` -> `docs/src/language-spec.md`
  mirror writer) was being run in *both* the `docs-audit` and `grammar-audit`
  lanes, which run in parallel — duplicating ~72s of work and racing two writers
  on the same mirror file. It now runs only in `docs-audit`; `grammar-audit`'s
  `verify_language_spec` reads the canonical spec source directly and does not
  depend on the mirror. On failure, the gate now prints a `RELEASE AUDIT FAILED`
  summary at the TOP of the output naming the failing lane *and the specific
  failing sub-step* (e.g. `grammar-audit / verify_tree_sitter_parse`), derived
  from the unmatched `time_phase` banner, plus the last 40 log lines — instead
  of forcing a maintainer to scroll thousands of lines into the full per-lane
  log dump (which is still emitted afterward for deep debugging).
