- Restore the legacy `std/cli` parser compatibility surface removed in v0.10.25; existing
  `parse_args` and `help_text` callers continue to work while new code uses `std/cli/argparse`.
- Keep the macOS toolchain-cache sandbox probe outside preset-writable temp aliases so it tests
  the intended cache/config boundary.
