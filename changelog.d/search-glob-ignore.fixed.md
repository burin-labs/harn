- Keep `hostlib_tools_search` glob filters inside the normal ignore-aware file
  walk so broad globs no longer re-include gitignored build output such as
  `target/`.
