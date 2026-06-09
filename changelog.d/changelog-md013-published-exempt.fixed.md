Release tooling no longer reflows already-published `CHANGELOG.md` sections.
`make lint-md` previously linted the assembled `CHANGELOG.md` (and `CHANGELOG-pre-*.md`
archives) under MD013 line-length, so long lines in published `## vX.Y.Z` sections were
flagged and rewrapped during a release — tripping the retroactive-edit guard. Those
machine-assembled, append-only files are now excluded from markdownlint; the
`changelog.d/*` fragments are still linted at the source.
