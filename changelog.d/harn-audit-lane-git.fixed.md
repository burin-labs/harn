The `Harn audit gates` CI lane can run git again, and two checks that read git
now fail instead of reporting a pass they never performed. Persisted checkout
credentials leave an `include.path` in `.git/config` pointing at a
`$RUNNER_TEMP` file the Harn sandbox cannot read, and git refuses to parse an
unreadable include — exiting 128 for every invocation. The lane checks out with
`persist-credentials: false` (it never pushes). The source file-length ratchet
reconciles its walk against `git ls-files`, so it lost that cross-check entirely
and still reported a pass; an unanswerable `git ls-files` is now a
`reconciliation_unavailable` finding carrying git's own message.
`verify_release_metadata` treated every failed git command as "no tag" and
returned clean, silently disabling the whole tag-state gate; it now resolves
HEAD first and fails with git's message when it cannot.
