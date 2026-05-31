- **ACP read-only sandbox roots for bundled embedder assets.**
  `AcpServerConfig::with_read_only_roots` lets an in-process ACP host register
  read-only sandbox roots outside the user's `workspace_roots`. The configured
  roots are unioned into the per-turn capability policy at
  `ModePolicyGuard::enter`, so `check_fs_path_scope` permits reads under them
  while still denying writes, deletes, and reads outside every root. This
  unblocks embedders (e.g. burin's Rust TUI) that ship bundled pipelines and
  their `@partials` outside the user's project tree.
