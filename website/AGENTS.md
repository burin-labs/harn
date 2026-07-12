# Working in `website/` (harnlang.com)

Read `README.md` for what this app is and how the content pipeline works. Read
`docs-stack-conventions.md` for the contract shared with the companion marketing
site. This file is the short list of conventions to hold to when editing the site.

## Setup and checks

```bash
npm ci             # once or when package-lock.json changes
npm run dev        # live dev server (reads ../docs/src live)
npm run typecheck  # tsc -b --noEmit
npm run test       # vitest
npm run build      # tsc + client + SSR + prerender into ../docs/dist
```

Run typecheck, test, and build before pushing. The build is the real integration
test: it prerenders every doc page through SSR, so a broken component or import
fails there even when dev looks fine.

## Conventions

- **No bare UI strings.** Every user-facing string lives in `src/i18n/en.ts`,
  grouped by surface. Read it with `useMessages()` in components or
  `getMessages()` in plain modules, and fill `{placeholders}` with `format()`.
  Adding a locale means adding one file and registering it in `src/i18n/index.ts`.
  Code identifiers, CLI commands, URLs, and file paths are not translatable and
  stay with their structural data, not in the catalog.
- **Small, single-responsibility components.** Page files compose sections; the
  sections live beside them (`src/components/landing/*`, `src/components/docs/*`).
  Prefer a new file over growing a 200-line component.
- **Write plainly.** Short sentences, active voice. Use colons or periods instead
  of em dashes. Skip the usual AI tells (rule-of-three filler, "not just X but Y",
  "it's worth noting"). See slopwash.com and Wikipedia's "Signs of AI writing".
- **Don't overpromise.** A CTA must do what it says. Nothing runs Harn in the
  browser today, so example actions link to source and docs rather than "Run".
- **Keep code snippets narrow.** No horizontal scroll. The example gallery strips
  each scenario's leading doc-comment (`leadWithCode`) so the code leads, and
  soft-wraps long lines. Keyword hover docs come from `src/lib/keyword-docs.ts`.

## Content

Docs content is Markdown under `../docs/src` (not in this app). Edit the source
there; this app only renders it. For language-spec changes, edit the relevant
`../spec/chapters/*.md` source and run `make sync-language-spec` from the repo
root. `../spec/HARN_SPEC.md` and `../docs/src/language-spec.md` are generated.
