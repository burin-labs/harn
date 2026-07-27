The reusable Harn runtime bump workflow now exposes a repository-owned
`refresh-command` before validation, so consumers with generated sources or
nonstandard dependency projections can use the canonical signed bump flow
without duplicating its release, branch, or pull-request orchestration. Refresh
failures now stop before validation or commit and are recorded explicitly in
the bump receipt. Callers whose owner commands require Node.js can request an
exact toolchain version without copying setup steps.
