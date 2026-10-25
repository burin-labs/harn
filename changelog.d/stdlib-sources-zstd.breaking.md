- **The embedded stdlib catalog is built on first read.** `harn_stdlib::CATALOG`
  is a `LazyLock<StdlibCatalog>`, and `harn_stdlib_catalog::install` takes a
  `&'static LazyCatalog`. The `harn_stdlib::STDLIB_SOURCES`,
  `STDLIB_PROMPT_ASSETS` and `STDLIB_CLI_SCRIPTS` constants are removed in
  favor of `CATALOG`'s fields. Hosts that call `harn_stdlib::install()` are
  unaffected. A startup check that only needs to know a catalog is installed
  calls `harn_stdlib_catalog::ensure_installed()`, which does not build it.

  Migration: read the lists through `CATALOG`, and wrap a hand-built catalog
  in `LazyCatalog` before installing it:

  ```rust
  // before
  for entry in harn_stdlib::STDLIB_SOURCES {}
  harn_stdlib_catalog::install(&MY_CATALOG)?; // static MY_CATALOG: StdlibCatalog
  // after
  for entry in harn_stdlib::CATALOG.sources {}
  harn_stdlib_catalog::install(&MY_CATALOG)?; // static MY_CATALOG: LazyCatalog
  ```
