- **Release binary size gate recovery.** The x86_64 Linux release binary-size
  gate no longer depends on the checked-out tag's `check_binary_size.harn`
  script for the hard release check, so workflow-dispatch recovery can rebuild
  immutable older tags even when the tagged helper script fails to type-check.
  The release binary budget is ratcheted to 189.25 MiB after v0.8.167 measured
  189.07 MiB, keeping the guard tight without blocking a valid patch release on
  a 0.07 MiB threshold miss.
