- **Release binary cache warms no longer get cancelled by no-op main pushes.**
  Scheduled and manual warm-cache runs now keep their own concurrency lane, and
  manual warm-cache dispatches can refresh default-branch release caches on
  `main`, reducing the chance that tag releases cold-build the slow macOS
  artifacts.
