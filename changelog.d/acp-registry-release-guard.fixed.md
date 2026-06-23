- **Release metadata now verifies the ACP registry manifest.** The release
  audit fails when `spec/acp-registry/harn/agent.json` or any binary archive URL
  drifts from the Cargo package version, preventing stale editor-install
  entries after release bumps.
