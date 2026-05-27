- **Towncrier-style changelog fragments.** PRs can now drop a single
  `changelog.d/<id>.<category>.md` file (categories: `breaking`, `added`,
  `changed`, `deprecated`, `removed`, `fixed`, `security`) instead of
  hand-editing `## Unreleased`. At release time the bump fleet's
  `release_harn.harn` assembles the fragments into the Unreleased block
  (preserving any operator-authored bullets) and stages the fragment files
  for deletion in the same release commit. Removes `## Unreleased` as a
  merge-conflict hot spot for parallel PRs. Direct edits to `CHANGELOG.md`
  remain accepted (legacy path). A soft `Changelog fragment` CI gate flags
  PRs that change user-visible code without a fragment; the
  `no-changelog-needed` label bypasses it.
