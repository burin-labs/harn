The `Harn audit gates` CI lane can run git again, and the source file-length
ratchet now says when it could not reconcile its walk. Persisted checkout
credentials leave an `include.path` in `.git/config` pointing at a
`$RUNNER_TEMP` file the Harn sandbox cannot read, and git refuses to parse an
unreadable include — exiting 128 for every invocation. `git ls-files` therefore
returned nothing, so the ratchet's cross-check for tracked sources the inventory
walk never reached was silently disabled while still reporting a pass. The lane
checks out with `persist-credentials: false` (it never pushes), and an
unanswerable `git ls-files` is now a `reconciliation_unavailable` finding
carrying git's own message rather than an empty list.
