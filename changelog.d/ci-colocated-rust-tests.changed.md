- CI: the environment-neutral Rust suite now executes inside the warm
  Behavior build leg instead of a standalone consumer lane, and the security
  proof consumes a single-package `harn-vm` archive instead of the ~8.6 GB
  workspace bundle. The multi-gigabyte behavior payload is no longer built or
  uploaded on the default path, removing 1.5-16.5 m of per-run artifact
  transfer variance. Kill switch: setting the repo variable
  `HARN_CI_DISABLE_COLOCATED_TESTS=true` restores the previous
  producer/consumer topology (standalone Rust test lane included) unchanged.
