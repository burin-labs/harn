- `std/bump/live` readiness and latest-release resolution now gate on a
  dedicated `release_repo` (default: the consumer repo; the reusable Harn
  runtime bump sets `burin-labs/harn` via `HARN_BUMP_RELEASE_REPO`). Previously
  both queried the consumer repo being bumped — which publishes no Harn
  releases — so every external fleet bump no-opped with "not fully published
  yet". Caught by the harn-latex canary.
