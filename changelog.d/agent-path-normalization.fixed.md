- `harn-hostlib` tool results now emit forward-slash-separated paths on every
  platform. Search matches, staged/committed/discarded file labels, command
  artifact paths (`output_path`/`stdout_path`/`stderr_path`), git repo roots,
  code-index roots, filesystem snapshot paths, and directory-watch events
  previously leaked OS-native backslashes on Windows (`crates\foo\bar.rs`),
  breaking the path invariant the model and every path-consuming pipeline
  assume. All agent-facing path strings now route through a single
  `to_agent_path` normalizer, guarded by `check_agent_path_normalization.sh`.
