- **Fixed four latent stdlib type bugs surfaced by precise record-merge typing.**
  Now that `merge` infers the exact merged shape, the type checker caught
  mismatches the old untyped `dict` return had hidden:
  - `github.enable_auto_merge` reads `method`/`merge_method` options that
    `GitHubCallOptions` never declared — added them.
  - `github.wait_until_deploy_succeeds` / `wait_until_ci_green` /
    `wait_until_pr_merged` built monitor options with GitHub's millisecond
    field names (`timeout_ms`, `poll_interval_ms`, `max_wait_ms`), which
    `wait_for` silently dropped — and since the monitor requires a `timeout`
    duration, those calls would have **thrown at runtime**. They now translate
    the millisecond cadence into the duration-typed `MonitorWaitOptions` so the
    caller's timing is honored.
  - The git-forge pull-request event builder is now annotated so its
    `filter_nil`-projected fields type as the declared `GitForge*` structs.
  - `graphql_parse_schema`'s `current` accumulator no longer trips a
    narrow-to-`never` reassignment error.
