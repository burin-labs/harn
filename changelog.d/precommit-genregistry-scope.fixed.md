- **Pre-commit release-workflow edits no longer compile Harn unnecessarily.**
  The generated-artifact registry hook now runs for the registry, Makefile,
  CI workflow, and hook surfaces it actually audits, rather than every
  workflow file.
