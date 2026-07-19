- **Task cancellation is now a resource-cleanup barrier.** `cancel(handle)` and
  the forced-abort branch of `cancel_graceful` wait for the aborted future to
  tear down its VM before returning, so owned channels, permits, host leases,
  and future resource guards cannot remain observably live after cancellation.
