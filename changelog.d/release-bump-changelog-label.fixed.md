- **Release bump PRs now satisfy the changelog-fragment gate automatically.**
  The legacy `release_ship.sh --bump` recovery path labels pure version-bump
  PRs with `no-changelog-needed` before enabling auto-merge, matching the
  gate's documented bypass for version-only release paperwork.
