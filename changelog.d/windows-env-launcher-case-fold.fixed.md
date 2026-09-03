- A policy-governed child process on Windows now inherits `PATH` and the rest
  of the environment allowlist. Windows reports the search path as `Path`
  (and other allowlisted names in their own casing), and the launcher
  snapshot was filtered by an exact-case match against the allowlist before
  the existing case-folding lookup ever saw the entry, so every allowlisted
  name silently dropped out of an isolated or granted child's environment on
  Windows even though the allowlist read as though it admitted them.
