`harn run` now honors `[check].trusted_host_dispatch` from `harn.toml`.
`check`, `lint`, and `test` already read the key, and `run` has no CLI flag to
compensate, so a project that declared the authority in its manifest still had
every `host_call` refused — including in the manifest's own trigger handlers,
which install before the script body runs and failed the whole invocation. The
manifest is also resolved from the absolute path, so a relative
`harn run scripts/main.harn` no longer runs out of ancestors before reaching
the repo-root manifest.
