import { describe, expect, it } from "vitest"
import { htmlPages } from "./prerender.mjs"

const template = `<!doctype html>
<html lang="en">
  <head>
    <title>Static title</title>
    <meta name="description" content="Static description" />
  </head>
  <body><div id="root"><!--app-html--></div></body>
</html>`

const doc = {
  slug: "getting-started",
  url: "/getting-started.html",
  title: "Getting started",
  description: "Get from zero to running your first Harn program.",
  html: "<h1>Getting started</h1>",
  headings: [],
  prev: null,
  next: null,
}

function metadata(kind, title, description, path) {
  const canonicalUrl = new URL(path, "https://harn.example").href
  return {
    kind,
    title,
    description,
    path,
    canonicalUrl,
    imageUrl: "https://harn.example/og-default.png",
  }
}

describe("prerender metadata", () => {
  it("emits distinct titles plus canonical and Open Graph metadata", () => {
    const pages = htmlPages({
      template,
      docs: { pages: new Map([[doc.slug, doc]]) },
      render: (url) => `<main>${url}</main>`,
      landingPageMeta: metadata("landing", "Harn", "Landing description.", "/"),
      notFoundPageMeta: metadata("notFound", "Page not found | Harn", "Missing page.", "/404.html"),
      pageMetaForDoc: (data) =>
        metadata("doc", `${data.title} | Harn`, data.description, data.url),
    })
    const htmlByPath = new Map(pages)
    const titleOf = (html) => html.match(/<title>(.*?)<\/title>/)?.[1]

    expect(titleOf(htmlByPath.get("index.html"))).not.toBe(
      titleOf(htmlByPath.get("getting-started.html")),
    )

    for (const [path, html] of pages) {
      expect(html, path).toContain('<meta property="og:title"')
      expect(html, path).toContain('<link rel="canonical"')
    }
  })
})
