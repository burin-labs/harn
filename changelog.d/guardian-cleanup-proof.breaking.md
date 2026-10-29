- **Linux command cleanup can prove custody independently of the host census.**
  A guardian reports `guardian_custody_complete` only after the kernel confirms
  that it has reaped every adopted descendant. The host census remains separately
  reported, including unreadable processes, and unknown or surviving children
  still prevent a clean result.

  Migration: consumers checking a zero `survivor_count` can accept either a
  complete host census or `guardian_custody_complete: true`, while continuing to
  reject omitted or surviving children. Rust callers constructing
  `ProcessCleanupReport` literals must initialize the new boolean to `false`
  unless they own the measured guardian proof.
