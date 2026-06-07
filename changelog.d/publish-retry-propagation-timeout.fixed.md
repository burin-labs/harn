- The release publish step (`scripts/publish.sh`) now treats cargo's "timeout
  while waiting for published dependencies" / "timed out waiting for … to be
  available" as a retryable index-propagation condition. Previously a slow
  crates.io index could leave the last crate (e.g. `harn-cli`, waiting on
  `harn-lsp`) unpublished and abort the run as a "non-retryable error" without
  even trying the per-crate fallback; it now retries and falls back, so a
  propagation lag no longer leaves a release half-published.
