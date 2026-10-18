- **A lean `harn-vm` embedder can now carry the precompiled stdlib.**
  `harn-stdlib-artifacts/embed` no longer forces `harn-vm/full`; select the same
  builtin families your `harn-vm` resolves to. `register()` now refuses a table
  compiled for another builtin contract with the new
  `EmbeddedStdlibRegistrationError::ForeignBuild` variant, instead of accepting a
  table every import would miss.

  Migration: add the families to the dependency, and handle the new variant in an
  exhaustive `match` on `EmbeddedStdlibRegistrationError`.

  ```toml
  # before
  harn-stdlib-artifacts = { version = "...", features = ["embed"] }
  # after: a full host
  harn-stdlib-artifacts = { version = "...", features = ["embed", "full"] }
  # after: a lean host whose harn-vm selects sqlite and native-keyring
  harn-stdlib-artifacts = { version = "...", features = ["embed", "sqlite", "native-keyring"] }
  ```
