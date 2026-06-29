- Standalone `host_call("project.metadata_*", ...)` now routes metadata get,
  inspect, set, save, stale, and refresh calls to Harn's built-in metadata
  store when no host bridge handles the call, and `harn check` recognizes the
  inspect operation by default. This preserves project metadata learning in
  CLI/debug runs.
