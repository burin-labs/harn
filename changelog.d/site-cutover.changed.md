- **harnlang.com is now a custom Vite + React + Tailwind site, replacing mdBook (full cutover).** The
  Diataxis-structured Markdown under `docs/src/` is unchanged — it is now rendered by a new app in `website/`
  with a Mintlify-style layout: a marketing landing page, full-width section tabs, a scoped sidebar, an
  on-this-page TOC, ⌘K full-text search, light/dark themes, and a redesigned look-and-feel (teal + amber brand).
  Harn code blocks are syntax-highlighted at build time using the same Rust-generated keyword table
  (`docs/theme/harn-keywords.js`), and every page is statically prerendered to crawlable HTML with the raw `.md`
  mirror and legacy redirects preserved. `scripts/build_docs_site.sh` now drives the Node build; mdBook
  (`book.toml`, the bundled theme JS/CSS) is removed. Render must build with `./scripts/build_docs_site.sh`
  (publish dir `docs/dist/`).
