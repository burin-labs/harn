- Make `harn lint --fix` exit non-zero when unfixable error-level diagnostics
  remain (and print them), matching the plain and `--json` lint paths so `--fix`
  in CI/pre-commit no longer passes green over real errors.
