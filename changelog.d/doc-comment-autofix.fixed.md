- **The public-function doc-comment auto-fixer now covers every wrong-format
  case and no longer double-reports.** A `pub fn` preceded by a `//` / `///`
  comment used to surface both the fixless `missing-harndoc` warning and the
  auto-fixable `legacy-doc-comment` one; `missing-harndoc` is now suppressed
  whenever an adjacent migratable comment exists, so you see a single
  fixable finding. Plain `/* … */` block comments (single- and multi-line)
  directly above a public item are now migrated to canonical `/** … */`
  too, matching the existing `//` handling.
