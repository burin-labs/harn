- **Release binary size gate recovery.** The x86_64 Linux release binary-size
  gate no longer depends on the checked-out tag's `check_binary_size.harn`
  script for the hard release check, so workflow-dispatch recovery can rebuild
  immutable older tags even when the tagged helper script fails to type-check.
