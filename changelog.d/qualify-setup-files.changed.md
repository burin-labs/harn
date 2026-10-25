- `scripts/precise_qualify.harn` now takes `-- <plan.json> <out-dir> [corpus-dir]`: an optional corpus directory
  resolves relative plan checkouts. Plans accept `setup_files`, text written into the scratch clone before setup,
  so a generated tsconfig can make the oracle cover sampled files that a repository's own configs skip.
