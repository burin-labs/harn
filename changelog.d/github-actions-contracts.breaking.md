- **GitHub Actions result contracts now match the released connector (#10370).**
  Runs expose `display_title` and `run_started_at`; jobs expose `head_sha`
  and typed `GitHubActionsStep` values. Dispatch receipts expose `created_at`.
  The types no longer promise `api_url`, job labels or runner groups, or
  dispatch status and conclusion, which the connector does not return.
  Read a dispatched run with `workflow_run` to obtain its status and conclusion.

  Migration: rename a run's `started_at` field to `run_started_at`, use `url`
  for links, and read dispatch status through `workflow_run`. Remove references
  to job labels and runner groups from these normalized result records.
