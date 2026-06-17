- **Agents API trigger-run observation.** `harn serve api` now exposes recent
  trigger-dispatched workflow runs at `/v1/workflow-trigger-runs`, including
  trigger outbox metadata joined with matching action-graph observations for
  Burin's local comment-trigger workflow surfaces (burin-code#2232).
