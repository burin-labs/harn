- **Runtime Git calls on macOS no longer pay the `xcrun` shim's lookup.** A
  confined server could not reach the shim's per-user cache, so every `git`
  the runtime ran for workspace checkpoints, scans and git tools looked up the
  toolchain again, about 0.7 s each and several seconds before a checkpointed
  prompt started. Session commands now run the selected developer Git
  (`DEVELOPER_DIR`, then `xcode-select`'s link, Command Line Tools, Xcode)
  whenever a PATH search would reach `/usr/bin/git`. A Git placed ahead of the
  shim on PATH still runs.
