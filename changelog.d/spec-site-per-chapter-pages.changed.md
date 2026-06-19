- **The hosted language specification is now navigable chapter-by-chapter.**
  `scripts/sync_language_spec.harn` generates a per-chapter page under
  `docs/src/spec/language/` for each section after the overview and rewrites the
  chapter list in `docs/src/SUMMARY.md`, so the site nav and search index cover
  every chapter instead of one ~7k-line page. `docs/src/language-spec.md` is now
  a landing page (the overview plus a table of contents); deep links to the old
  monolithic anchors were repointed to their chapter pages. The single-file
  `spec/HARN_SPEC.md` assembly is unchanged, and the per-chapter
  `spec/chapters/*.md` sources remain the one place to edit.
