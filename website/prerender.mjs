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

const MANAGED_META_NAMES = ["twitter:card"]
const MANAGED_META_PROPERTIES = ["og:title", "og:description", "og:type", "og:url", "og:image"]

function regexAlternatives(values) {
  return values.map((value) => value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join("|")
}

function escapeHtml(value) {
  return String(value).replace(/[&<>"']/g, (ch) => {
    switch (ch) {
      case "&":
        return "&amp;"
      case "<":
        return "&lt;"
      case ">":
        return "&gt;"
      case '"':
        return "&quot;"
      default:
        return "&#39;"
    }
  })
}

function safeJson(value) {
  return JSON.stringify(value).replace(/</g, "\\u003c")
}

function stripManagedHeadTags(html) {
  const metaNames = regexAlternatives(MANAGED_META_NAMES)
  const metaProperties = regexAlternatives(MANAGED_META_PROPERTIES)
  return html
    .replace(/\s*<link\b(?=[^>]*\brel=["']canonical["'])[^>]*>/gi, "")
    .replace(
      new RegExp(`\\s*<meta\\b(?=[^>]*\\bname=["'](?:${metaNames})["'])[^>]*>`, "gi"),
      "",
    )
    .replace(
      new RegExp(
        `\\s*<meta\\b(?=[^>]*\\bproperty=["'](?:${metaProperties})["'])[^>]*>`,
        "gi",
      ),
      "",
    )
    .replace(
      /\s*<script\b(?=[^>]*\btype=["']application\/ld\+json["'])[^>]*>[\s\S]*?<\/script>/gi,
      "",
    )
}

function rewriteTitleAndDescription(html, meta) {
  const title = `<title>${escapeHtml(meta.title)}</title>`
  const description = `<meta name="description" content="${escapeHtml(meta.description)}" />`
  const withTitle = /<title>[\s\S]*?<\/title>/i.test(html)
    ? html.replace(/<title>[\s\S]*?<\/title>/i, title)
    : html.replace(/<head\b[^>]*>/i, (tag) => `${tag}\n    ${title}`)

  if (/<meta\b(?=[^>]*\bname=["']description["'])[^>]*>/i.test(withTitle)) {
    return withTitle.replace(/<meta\b(?=[^>]*\bname=["']description["'])[^>]*>/i, description)
  }
  return withTitle.replace(title, `${title}\n    ${description}`)
}

function jsonLd(meta) {
  if (meta.kind === "landing") {
    return {
      "@context": "https://schema.org",
      "@type": "SoftwareApplication",
      name: "Harn",
      applicationCategory: "DeveloperApplication",
      operatingSystem: "macOS, Linux, Windows",
      description: meta.description,
      url: meta.canonicalUrl,
      image: meta.imageUrl,
      offers: {
        "@type": "Offer",
        price: "0",
        priceCurrency: "USD",
      },
    }
  }
  if (meta.kind === "doc") {
    return {
      "@context": "https://schema.org",
      "@type": "TechArticle",
      headline: meta.title,
      description: meta.description,
      url: meta.canonicalUrl,
      image: meta.imageUrl,
    }
  }
  return {
    "@context": "https://schema.org",
    "@type": "WebPage",
    name: meta.title,
    description: meta.description,
    url: meta.canonicalUrl,
  }
}

function rewriteHead(html, meta) {
  const withTitle = rewriteTitleAndDescription(html, meta)
  const withoutManaged = stripManagedHeadTags(withTitle)
  const ogType = meta.kind === "doc" ? "article" : "website"
  const tags = [
    `<link rel="canonical" href="${escapeHtml(meta.canonicalUrl)}" />`,
    `<meta property="og:title" content="${escapeHtml(meta.title)}" />`,
    `<meta property="og:description" content="${escapeHtml(meta.description)}" />`,
    `<meta property="og:type" content="${ogType}" />`,
    `<meta property="og:url" content="${escapeHtml(meta.canonicalUrl)}" />`,
    `<meta property="og:image" content="${escapeHtml(meta.imageUrl)}" />`,
    `<meta name="twitter:card" content="summary_large_image" />`,
    `<script type="application/ld+json">${safeJson(jsonLd(meta))}</script>`,
  ].join("\n    ")
  return withoutManaged.replace("</head>", `    ${tags}\n  </head>`)
}

export function page(template, appHtml, payload, meta) {
  let html = rewriteHead(template.replace("<!--app-html-->", appHtml), meta)
  if (payload) {
    const json = safeJson(payload)
    html = html.replace(
      "</body>",
      `  <script id="__HARN_PAGE__" type="application/json">${json}</script>\n  </body>`,
    )
  }
  return html
}

export function htmlPages({
  template,
  docs,
  render,
  landingPageMeta,
  notFoundPageMeta,
  pageMetaForDoc,
}) {
  const pages = [["index.html", page(template, render("/"), null, landingPageMeta)]]
  for (const [slug, data] of docs.pages) {
    const url = "/" + slug + ".html"
    pages.push([slug + ".html", page(template, render(url, data), data, pageMetaForDoc(data))])
  }
  pages.push([
    "404.html",
    page(template, render("/this-page-does-not-exist"), null, notFoundPageMeta),
  ])
  return pages
}

export async function prerenderSite() {
  const {
    render,
    loadAllDocs,
    REPO_ROOT,
    LANDING_PAGE_META,
    NOT_FOUND_PAGE_META,
    pageMetaForDoc,
  } = await import("./.ssr/entry-server.js")

  const template = readFileSync(join(DIST, "index.html"), "utf8")
  if (!template.includes("<!--app-html-->")) {
    throw new Error("prerender: index.html is missing the <!--app-html--> placeholder")
  }

  const docs = loadAllDocs(REPO_ROOT)
  const writeFile = (relPath, contents) => {
    const full = join(DIST, relPath)
    mkdirSync(dirname(full), { recursive: true })
    writeFileSync(full, contents)
  }

  // Per-page content JSON (fetched on client navigation) + search index.
  for (const [slug, data] of docs.pages) {
    writeFile(`_content/${slug}.json`, JSON.stringify(data))
  }
  writeFile("_content/search.json", JSON.stringify(docs.search))

  const pages = htmlPages({
    template,
    docs,
    render,
    landingPageMeta: LANDING_PAGE_META,
    notFoundPageMeta: NOT_FOUND_PAGE_META,
    pageMetaForDoc,
  })
  for (const [relPath, contents] of pages) {
    writeFile(relPath, contents)
  }

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

  // sitemap.xml from the canonical URLs the pages already declare, so search
  // engines can discover every page. robots.txt (in public/) points here.
  const sitemapUrls = [LANDING_PAGE_META.canonicalUrl]
  for (const [, data] of docs.pages) {
    sitemapUrls.push(pageMetaForDoc(data).canonicalUrl)
  }
  writeFile(
    "sitemap.xml",
    `<?xml version="1.0" encoding="UTF-8"?>\n` +
      `<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n` +
      sitemapUrls.map((url) => `  <url><loc>${escapeHtml(url)}</loc></url>`).join("\n") +
      `\n</urlset>\n`,
  )

  console.log(
    `prerender: ${docs.pages.size} doc pages + landing + 404, ${docs.pages.size} content JSON, ` +
      `${Object.keys(REDIRECTS).length} redirects, sitemap.xml (${sitemapUrls.length} urls) → docs/dist`,
  )
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  await prerenderSite()
}
