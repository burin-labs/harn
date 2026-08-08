# `changelog.d/` — changelog fragments

Each non-trivial PR drops a single markdown file in this directory. At release
time `release_harn.harn` (in `~/projects/harn-bump-fleet`) reads every fragment,
groups them by category, folds them into the top `## Unreleased` block in
`../CHANGELOG.md`, then deletes the fragments in the same release commit.

The pattern is a small adaptation of [towncrier](https://towncrier.readthedocs.io).
It exists to remove `## Unreleased` as a merge-conflict hot spot. Two PRs that
both add a bullet to `## Unreleased` will conflict on every line they touch;
two PRs that each add their own `2491.added.md` / `2492.breaking.md` fragment
file never touch the same path.

## File format

```text
changelog.d/<id>.<category>.md
```

- **`<id>`** — the PR or issue number this fragment describes. Numeric is best
  (sorts deterministically inside a category); slugs like `migration-2026-05`
  are accepted too. The id is *not* rendered into the published changelog — it
  exists solely to make filenames unique.
- **`<category>`** — one of: `breaking`, `added`, `changed`, `deprecated`,
  `removed`, `fixed`, `security`. Maps 1:1 to a `### Heading` in the assembled
  section, emitted in the canonical Keep-a-Changelog order shown above.
- **`.md`** — must be markdown. Files that don't match the
  `<id>.<category>.md` shape (including this `README.md` and the
  `.gitkeep`) are ignored by the assembler.

## Body format

The body is the bullet(s) that would otherwise be hand-typed under
`### Breaking` / `### Added` / etc. in `CHANGELOG.md`. Do not include the
`### Heading` line itself — the assembler adds it. Multiple bullets per
fragment are fine.

Example: `changelog.d/2492.breaking.md`

```markdown
- **The typechecker now enforces declared generic and return contracts more
  strictly (#2492).** Generic type parameters are no longer treated as
  wildcard-compatible values, typed functions and pipelines must satisfy
  their declared return types across bare returns, fallthrough, nested
  returns, and exhaustive final matches, and top-level forward
  placeholders promote to their concrete binding types.
```

Example: `changelog.d/2493.fixed.md`

```markdown
- **Unused-variable lint now recognizes `parallel ... with { ... }` option
  expressions (#2493).** Locals used only in options such as
  `max_concurrent` are no longer reported as unused.
```

At release time these become:

```markdown
## v0.8.46

### Breaking

- **The typechecker now enforces declared generic and return contracts...**

### Fixed

- **Unused-variable lint now recognizes `parallel ... with { ... }`...**
```

## Authoring guidance

- One fragment per logical change. A PR that ships a feature *and* a related
  fix is welcome to land two fragments (e.g. `2494.added.md` and
  `2494.fixed.md`). The two-segment filename naturally supports this.
- Lead with **bold** project-area framing the way existing `CHANGELOG.md`
  entries do (e.g. `**Built-in clone, deep_clone, deep_merge.**` …). The
  assembler preserves your markdown verbatim.
- Cite the PR or issue number in the body too (`(#2492)`), not just in the
  filename. Filenames are not rendered into the published changelog;
  in-body citations are how the reader navigates back to the source.
- Operator-authored bullets typed directly into `## Unreleased` still work
  and are preserved at promotion time (they merge with fragment-derived
  bullets under matching subsections). Fragments are the preferred path
  precisely because they avoid the merge-conflict surface, but the
  in-place path is intentionally not removed.

## Bypass

PRs that genuinely do not need a changelog entry (typo fixes, internal
refactors, CI tweaks, dependency bumps that have no user-visible effect)
carry the `no-changelog-needed` label. The
`.github/workflows/changelog-fragment-check.yml` soft gate accepts that
label as the bypass, mirroring the `no-demo-needed` pattern.
