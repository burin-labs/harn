- **`harn lint --strict` promotes lint warnings to a non-zero exit.** The flag
  overrides `[check] strict` in `harn.toml`, so a single invocation can deny
  warning noise (e.g. in CI) instead of leaving every finding advisory. The
  bundled demo scenarios and repo `scripts/*.harn` lint clean under it, and
  `make lint-harn` now runs those surfaces with `--strict` so lint noise cannot
  regress.
