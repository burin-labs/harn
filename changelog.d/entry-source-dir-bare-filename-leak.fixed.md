- `harn run <bare-filename>` from a project root now resolves top-level
  `@asset`/relative prompt paths against the project even when a
  `[dependencies]` provider connector is installed. The entry pipeline's source
  dir is now always established (falling back to the working directory) and
  re-asserted immediately before execution, so a dependency provider-connector
  contract load during startup can no longer leave the resting source dir
  pointing at `.harn/packages/<dep>/`.
