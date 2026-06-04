- **`harn fmt`, `harn lint --fix`, and the LSP on-save fixer share one autofix
  apply/dedup policy.** The "drop overlapping fixes and splice right-to-left"
  logic now lives in one place (`FixEdit::apply_all` / `dedupe_overlapping`), so
  the three surfaces can no longer drift on which conflicting fixes win.
