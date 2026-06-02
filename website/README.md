# harnlang.com

The marketing site and documentation for [harnlang.com](https://harnlang.com) — a custom
Vite + React + Tailwind app that renders the Diataxis-structured Markdown in `../docs/src`.
It replaced mdBook in #2922.

The content stays in `docs/src/` (so every generator and checker — `check-docs-snippets`, the
language-spec mirror, diagnostics, `harn-keywords.js` — is unaffected). This app is only the
rendering layer.

## Develop

```bash
npm install
npm run dev
```

The dev server reads `../docs/src/**/*.md` live; edits to docs or to the app hot-reload.

## Build

```bash
npm run build
```

This runs `tsc`, the Vite client build, the Vite SSR build, and `prerender.mjs`, emitting a
statically prerendered, fully crawlable site into `../docs/dist`. The canonical build entry point
is `../scripts/build_docs_site.sh`, which also mirrors the raw `.md` sources and the LLM
quick-reference files into `docs/dist`.

```bash
npm run typecheck   # tsc -b --noEmit
```

## How it works

- **Content pipeline** (`vite-plugins/content.ts`): globs `docs/src/**/*.md`, resolves mdBook
  `{{#include}}` directives, rewrites intra-doc links to `.html` URLs, parses `SUMMARY.md` into the
  nav tree + section tabs, and renders Markdown to HTML with `unified`/`remark`/`rehype`.
- **Harn highlighting**: a grammar ported from the old `harn-hljs.js`, fed by the Rust-generated
  `docs/theme/harn-keywords.js` (regenerate with `make gen-highlight`).
- **Search**: a client-side index (`_content/search.json`) fetched on first ⌘K.
- **SSG** (`prerender.mjs`): renders every route to a real HTML file, embeds per-page JSON for
  hydration, and emits the legacy redirect stubs.

## Layout

| Path | Purpose |
| --- | --- |
| `index.html` | App shell + pre-hydration theme script |
| `src/pages/` | `LandingPage`, `DocPage`, `DocRoute`, `NotFound` |
| `src/layouts/RootLayout.tsx` | Navbar, footer, ⌘K search |
| `src/components/` | Navbar, Footer, Logo, ThemeToggle, SearchModal, HarnMockup |
| `vite-plugins/` | Build-time content pipeline + virtual modules |
| `prerender.mjs` | Static-site generation |

## Deploy

Pushes to `main` that touch `docs/` or `website/` fire the `deploy-docs` CI job, which calls the
Render deploy hook for the `harn-docs` static site (build command `./scripts/build_docs_site.sh`,
publish directory `docs/dist`).
