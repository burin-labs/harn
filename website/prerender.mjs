// Static-site generation.
//
// After `vite build` (client → docs/dist) and `vite build --ssr` (server →
// .ssr), this renders every route to a real HTML file so the docs are fully
// crawlable and work with JavaScript disabled — no host rewrite rules needed.
// It also writes the per-page / search JSON the client fetches on navigation,
// and the legacy redirect stubs that preserve old mdBook URLs.
import { mkdirSync, writeFileSync, readFileSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"

const here = dirname(fileURLToPath(import.meta.url))
const DIST = join(here, "../docs/dist")

const { render, loadAllDocs, REPO_ROOT } = await import("./.ssr/entry-server.js")

const template = readFileSync(join(DIST, "index.html"), "utf8")
if (!template.includes("<!--app-html-->")) {
  throw new Error("prerender: index.html is missing the <!--app-html--> placeholder")
}

const docs = loadAllDocs(REPO_ROOT)

function writeFile(relPath, contents) {
  const full = join(DIST, relPath)
  mkdirSync(dirname(full), { recursive: true })
  writeFileSync(full, contents)
}

function page(appHtml, payload) {
  let html = template.replace("<!--app-html-->", appHtml)
  if (payload) {
    const json = JSON.stringify(payload).replace(/</g, "\\u003c")
    html = html.replace(
      "</body>",
      `  <script id="__HARN_PAGE__" type="application/json">${json}</script>\n  </body>`,
    )
  }
  return html
}

// Per-page content JSON (fetched on client navigation) + search index.
for (const [slug, data] of docs.pages) {
  writeFile(`_content/${slug}.json`, JSON.stringify(data))
}
writeFile("_content/search.json", JSON.stringify(docs.search))

// Landing page.
writeFile("index.html", page(render("/"), null))

// One static HTML file per doc page.
let count = 0
for (const [slug, data] of docs.pages) {
  const url = "/" + slug + ".html"
  writeFile(slug + ".html", page(render(url, data), data))
  count++
}

// 404 — Render serves this for unmatched paths.
writeFile("404.html", page(render("/this-page-does-not-exist"), null))

// Legacy redirects carried over from the mdBook book.toml so old inbound links
// keep working after the cutover.
const REDIRECTS = {
  "docs/prompt-templating/index.html": "/prompt-templating.html",
  "docs/prompt-templating.html": "/prompt-templating.html",
  "typed-tools.html": "/llm/tools.html",
  "docs/typed-tools.html": "/llm/tools.html",
  "providers.html": "/llm/providers.html",
  "docs/providers.html": "/llm/providers.html",
}
for (const [from, to] of Object.entries(REDIRECTS)) {
  writeFile(
    from,
    `<!doctype html><html><head><meta charset="utf-8">` +
      `<title>Redirecting…</title>` +
      `<meta http-equiv="refresh" content="0; url=${to}">` +
      `<link rel="canonical" href="${to}">` +
      `<script>location.replace(${JSON.stringify(to)})</script>` +
      `</head><body>Redirecting to <a href="${to}">${to}</a>…</body></html>`,
  )
}

console.log(
  `prerender: ${count} doc pages + landing + 404, ${docs.pages.size} content JSON, ` +
    `${Object.keys(REDIRECTS).length} redirects → docs/dist`,
)
