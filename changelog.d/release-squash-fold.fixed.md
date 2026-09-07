- **Post-release development bumps now recognize squash-merged release commits.**
  Publishing a certified release whose pull request was squash-merged into
  `main` now advances the workspace to the next `-dev` version instead of
  incorrectly treating the release tag as unrelated history.
