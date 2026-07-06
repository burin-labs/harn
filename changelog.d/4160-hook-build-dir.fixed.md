- **Hook Harn builds now keep Cargo build artifacts inside the isolated hook
  target directory (#4160).** Local hooks default `CARGO_BUILD_BUILD_DIR` under
  `CARGO_TARGET_DIR` so warm Harn CLI builds no longer spill into the shared
  cargo build directory.
