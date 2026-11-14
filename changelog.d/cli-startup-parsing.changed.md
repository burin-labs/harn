CLI startup parses arguments once and reuses the result for runtime selection,
Linux server confinement, and dispatch. Help and argument errors return before
starting the async runtime.
Runtime assets are warmed after argument parsing, so help and version flags
avoid execution setup while preserving startup configuration validation.
