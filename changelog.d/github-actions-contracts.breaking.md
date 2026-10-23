- The GitHub Actions stdlib result types now match the released connector.
  Runs expose `display_title` and `run_started_at`; jobs expose `head_sha`
  and typed `GitHubActionsStep` values. Dispatch receipts expose `created_at`.
  The types no longer promise `api_url`, job labels or runner groups, or
  dispatch status and conclusion, which the connector does not return.
  Read a dispatched run with `workflow_run` to obtain its status and conclusion.
