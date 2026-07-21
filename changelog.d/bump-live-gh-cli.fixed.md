- Fixed `std/bump/live` so the reusable `bump-harn` workflow can actually reach
  GitHub. It routed every GitHub call through `std/connectors/github`, whose
  `connector_call("github", …)` needs an ACTIVE connector client — only installed
  in hosted/orchestrator runtimes, never in the plain `harn run` the workflow
  uses — so every downstream bump threw `connector 'github' is not active`. The
  adapter now drives GitHub through the `gh` CLI (App token via GH_TOKEN, patched
  into the environment so PATH/HOME survive), matching how the rest of the fleet
  reaches GitHub. Signed commits still go through the GraphQL
  `createCommitOnBranch` mutation.
