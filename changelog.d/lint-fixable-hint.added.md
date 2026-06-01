- **`harn lint` and `harn fmt --check` now point you at the auto-fix flag.**
  When findings are machine-fixable, lint prints an ESLint-style summary
  ("All N finding(s) are auto-fixable — run `harn lint --fix` to apply them.",
  or "M of N …" when only some are), and `fmt --check` reports that N files
  would be reformatted and to re-run `harn fmt` without `--check`. The hint is
  stderr-only and never prints once the fixes have been applied; the `--json`
  report shape is unchanged.
