- **Provider commands are consolidated under a single `harn provider` noun.**
  The six top-level commands `providers`, `provider`, `provider-catalog`,
  `provider-ready`, `provider-probe`, and `provider-tool-probe` are gone.
  Use `harn provider capabilities`, `harn provider catalog <refresh|validate|
  build-config|build-capabilities|export|matrix|support|recommend|show>`
  (`show` prints the loaded catalog JSON, formerly `provider-catalog`),
  `harn provider ready`, `harn provider probe`, and `harn provider tool-probe`.
