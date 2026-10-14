- **Hosts install the standard library.** `harn-vm` and `harn-modules` no
  longer depend on `harn-stdlib`; they read the embedded stdlib through the new
  `harn-stdlib-catalog` crate, so editing a stdlib `.harn` file rebuilds only
  `harn-stdlib` and the binaries that link it instead of the compiler and every
  crate built on it. `harn_vm::initialize_runtime()` now fails with
  `RuntimeInitError::StdlibCatalog` when no catalog is installed, and reading
  the stdlib without one panics with the same message rather than finding an
  empty library. The standalone `harn-lsp` and `harn-dap` shims build only with
  their default `standalone` feature; the shipped `harn` binary is unchanged.

  Migration: Rust embedders add a `harn-stdlib` dependency and call
  `harn_stdlib::install()` before `harn_vm::initialize_runtime()`. Test crates
  can instead enable `harn-modules`' `bundled-stdlib` feature as a
  dev-dependency, which installs the bundled catalog on first read:

  ```rust
  harn_stdlib::install().expect("install the Harn stdlib");
  harn_vm::initialize_runtime()?;
  ```
