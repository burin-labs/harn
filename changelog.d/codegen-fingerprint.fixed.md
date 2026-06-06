- **Bytecode cache fingerprints are now checkout-path stable.** Harn no longer
  bakes absolute compiler source paths into `HARN_CODEGEN_FINGERPRINT`, so
  precompiled `.harnbc` artifacts generated from the same Harn source match
  across Linux, macOS, and separate local worktrees.
