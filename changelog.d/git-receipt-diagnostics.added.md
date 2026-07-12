The `std/git` receipt commands (`git.diff`, `git.status`, and friends) now emit a one-line `[harn] warning:`
diagnostic when a git subprocess fails, naming the operation, exit code, and first line of stderr. These
commands still never throw and return a `success=false` receipt, but the failure is no longer silent — a broken
environment (e.g. running in a non-repo directory) is now visible at the source instead of only surfacing as an
empty diff or status downstream. Warnings deduplicate per `(operation, exit_code, first-stderr-line)` so probe
loops cannot spam, and the `git.repo.discover` probe stays silent because a failed discovery is expected control
flow.
