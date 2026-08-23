import { dirname, resolve } from "node:path"
import { fileURLToPath } from "node:url"
import { beforeAll, describe, expect, it } from "vitest"

import { loadAllDocs, type LoadedDocs } from "./content.ts"
import {
  DIAGRAM_CANVAS_CLASS,
  DIAGRAM_FIGURE_CLASS,
  DIAGRAM_SOURCE_CLASS,
  DIAGRAM_SOURCE_LABEL,
} from "../src/lib/diagram-markup.ts"

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..")

let docs: LoadedDocs

beforeAll(() => {
  docs = loadAllDocs(repoRoot)
}, 120_000)

describe("documentation content contract", () => {
  it("indexes every page and resolves every internal page and anchor link", () => {
    // loadAllDocs throws on a dangling page, link, or anchor; reaching beforeAll
    // without throwing is the assertion.
    expect(docs.pages.size).toBeGreaterThan(0)
  })
})

describe("diagram rendering", () => {
  const pagesWithDiagrams = () =>
    [...docs.pages.values()].filter((p) => p.html.includes(DIAGRAM_FIGURE_CLASS))

  it("turns every ```mermaid fence into a diagram figure, not a bare code block", () => {
    for (const page of docs.pages.values()) {
      // Inside a figure the source is the accessible fallback; anywhere else it
      // is the bug this guards — a diagram published as its own source.
      const outsideFigures = page.html.replace(
        new RegExp(`<figure class="${DIAGRAM_FIGURE_CLASS}">[\\s\\S]*?</figure>`, "g"),
        "",
      )
      expect(
        outsideFigures,
        `${page.slug} still renders a mermaid fence as source`,
      ).not.toMatch(/language-mermaid/)
    }
    expect(pagesWithDiagrams().length).toBeGreaterThan(0)
  })

  it("keeps the diagram source in the figure as a text alternative", () => {
    const page = docs.pages.get("concepts/mental-model")
    expect(page).toBeDefined()
    const figure = new RegExp(
      `<figure class="${DIAGRAM_FIGURE_CLASS}">[\\s\\S]*?</figure>`,
    ).exec(page!.html)?.[0]
    expect(figure).toBeDefined()
    // An empty canvas for the browser-rendered SVG…
    expect(figure).toContain(`<div class="${DIAGRAM_CANVAS_CLASS}" tabindex="0"></div>`)
    // …and the source behind a <details> the renderer reads back.
    expect(figure).toContain(`<details class="${DIAGRAM_SOURCE_CLASS}">`)
    expect(figure).toContain(`<summary>${DIAGRAM_SOURCE_LABEL}</summary>`)
    expect(figure).toContain("flowchart TD")
  })

  it("keeps diagram markup out of the search index", () => {
    const entry = docs.search.find((d) => d.slug === "concepts/mental-model")
    expect(entry).toBeDefined()
    expect(entry!.text).not.toContain("flowchart TD")
    expect(entry!.text).toContain("Harn program")
  })
})

describe("syntax highlighting", () => {
  const highlightedSample = (language: string) => {
    for (const page of docs.pages.values()) {
      const block = new RegExp(
        `<code class="hljs language-${language}">([\\s\\S]*?)</code>`,
      ).exec(page.html)
      if (block && block[1].includes("hljs-")) return { slug: page.slug, body: block[1] }
    }
    return null
  }

  // Registering only the Harn grammar with rehype-highlight replaces (rather
  // than extends) lowlight's default set, which silently drops highlighting for
  // every other language. These are the languages docs/src actually uses.
  it.each(["harn", "bash", "json", "toml", "rust", "typescript", "yaml", "powershell"])(
    "highlights %s code blocks",
    (language) => {
      expect(highlightedSample(language), `no highlighted ${language} block found`).not.toBeNull()
    },
  )

  it("highlights harn-prompt directives with the generated template vocabulary", () => {
    const sample = highlightedSample("harn-prompt")
    expect(sample).not.toBeNull()
    expect(sample!.body).toContain("hljs-template-variable")
  })

  it("highlights jsonl blocks through the json alias", () => {
    expect(highlightedSample("jsonl")).not.toBeNull()
  })
})
