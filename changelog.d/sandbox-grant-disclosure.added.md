- `harn run` / `harn time run` now print one precise stderr line naming the
  delta when a `--write-root` or `--read-only-root` grant (or
  `--allow-process-network`) widens the still-active sandbox, e.g.
  `sandbox active; extra write root: /path`. Routine grant-scoped runs no
  longer emit the blanket `--no-sandbox` filesystem/process/egress warning,
  which stays reserved for the full `--no-sandbox` escape hatch.
