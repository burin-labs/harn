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
import { CODE_FIGURE_CLASS, CODE_FILENAME_CLASS } from "../src/lib/code-markup.ts"
import { SYSTEMS, CAPABILITIES, RATINGS } from "../src/data/comparison.ts"
import {
  DIAGNOSTIC_DETAIL_CLASS,
  DIAGNOSTIC_FIGURE_CLASS,
  DIAGNOSTIC_REPAIR_CLASS,
  DIAGNOSTIC_SPAN_CLASS,
} from "../src/lib/diagnostic-markup.ts"

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

  it("keeps include-resolved Markdown on every page for the agent projection", () => {
    expect(docs.pages.size).toBeGreaterThan(0)
    for (const page of docs.pages.values()) {
      expect(page.markdownSource.length, page.slug).toBeGreaterThan(0)
    }
  })

  it("keeps the row label visible while wide comparison tables scroll", () => {
    const featureMatrix = docs.pages.get("feature-matrix")
    const precedence = docs.pages.get("spec/language/03-operator-precedence-table")
    expect(featureMatrix).toBeDefined()
    expect(precedence).toBeDefined()
    expect(featureMatrix!.html).toContain('class="table-scroll table-scroll-wide"')
    expect(precedence!.html).toContain('class="table-scroll"')
    expect(precedence!.html).not.toContain("table-scroll-wide")
  })
})

describe("comparison matrix", () => {
  const page = () => docs.pages.get("feature-matrix")!

  it("expands the directive into a real Markdown table, not a literal", () => {
    expect(page().markdownSource).not.toContain("{{#comparison-matrix}}")
    // The agent projection has to be a Markdown table, not raw HTML: it is what
    // an LLM reads and what the search index is built from.
    for (const system of SYSTEMS) {
      expect(page().markdownSource).toContain(`| ${system.name} |`)
    }
  })

  it("ships every column in the HTML so the picker is only an enhancement", () => {
    // A reader with JavaScript off, and the prerendered page a crawler sees,
    // must get the whole comparison — the hook narrows it, it does not build it.
    for (const system of SYSTEMS) {
      expect(page().html, system.id).toContain(`data-system="${system.id}"`)
    }
  })

  it("anchors every row at a heading that exists on the page", () => {
    // The row label links to `#<capability id>`. If a heading is renamed
    // without renaming the id, the link dies silently — this is that guard.
    for (const cap of CAPABILITIES) {
      expect(
        page().headings.some((h) => h.id === cap.id),
        `capability "${cap.id}" has no matching heading in feature-matrix.md`,
      ).toBe(true)
    }
  })

  it("rates every system on every capability", () => {
    for (const cap of CAPABILITIES) {
      for (const system of SYSTEMS) {
        expect(RATINGS[cap.id]?.[system.id], `${cap.id} × ${system.id}`).toBeDefined()
      }
    }
  })

  it("keeps rows Harn does not win, and labels exactly those", () => {
    // A comparison published by Harn in which Harn wins every row is an
    // advertisement. Guard the property, not the current row count.
    const harnLoses = CAPABILITIES.filter((c) => RATINGS[c.id]?.harn?.rating !== "yes")
    expect(harnLoses.length).toBeGreaterThanOrEqual(3)

    // Bidirectional: upgrading Harn's rating without unmarking the row, or
    // marking a row Harn actually wins, both fail here. A one-way check would
    // let the page drift back to an all-Yes Harn column unnoticed.
    expect(harnLoses.map((c) => c.id).sort()).toEqual(
      CAPABILITIES.filter((c) => c.favorsOthers)
        .map((c) => c.id)
        .sort(),
    )
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

describe("code block filenames", () => {
  const introduction = () => {
    const page = docs.pages.get("introduction")
    if (!page) throw new Error("introduction page missing")
    return page.html
  }

  it("renders a titled fence as a figure captioned with the filename", () => {
    const html = introduction()
    expect(html).toContain(`class="${CODE_FIGURE_CLASS}"`)
    expect(html).toMatch(
      new RegExp(`<figcaption class="${CODE_FILENAME_CLASS}">example\\.harn</figcaption>`),
    )
  })

  it("keeps the caption before the block it names", () => {
    const figure = introduction().match(
      new RegExp(`<figure class="${CODE_FIGURE_CLASS}">([\\s\\S]*?)</figure>`),
    )
    expect(figure).not.toBeNull()
    const inner = figure![1]
    expect(inner.indexOf("<figcaption")).toBeLessThan(inner.indexOf("<pre"))
  })

  it("does not leak the title into the rendered code or the language class", () => {
    const html = introduction()
    // The fence reads ```harn,check title="example.harn". None of the metadata
    // may survive as text, and the language class stays the bare language.
    expect(html).not.toContain('title="example.harn"</')
    expect(html).not.toContain("language-harn,check")
    expect(html).toContain("language-harn")
    expect(html).not.toContain("data-file")
  })

  it("leaves an untitled fence as a bare pre", () => {
    // Some page other than the introduction still has plain blocks; none of
    // them should have picked up a figure wrapper.
    const bare = [...docs.pages.values()].filter(
      (p) => p.html.includes("<pre") && !p.html.includes(CODE_FIGURE_CLASS),
    )
    expect(bare.length).toBeGreaterThan(0)
  })
})

describe("checked diagnostic examples", () => {
  const languageBasics = () => {
    const page = docs.pages.get("language-basics")
    if (!page) throw new Error("language-basics page missing")
    return page.html
  }

  it("renders compiler and linter spans from the checked projection", () => {
    const html = languageBasics()
    expect(html.match(new RegExp(`class="[^"]*${DIAGNOSTIC_FIGURE_CLASS}`, "g"))).toHaveLength(2)
    expect(html).toContain(`${DIAGNOSTIC_SPAN_CLASS}-error`)
    expect(html).toContain(`${DIAGNOSTIC_SPAN_CLASS}-warning`)
    expect(html).toContain("HARN-TYP-007")
    expect(html).toContain("HARN-LNT-018")
  })

  it("keeps help and registry repair text visible without hover", () => {
    const html = languageBasics()
    expect(html).toContain(`class="${DIAGNOSTIC_DETAIL_CLASS}`)
    expect(html).toContain("Help:")
    expect(html).toContain(`class="${DIAGNOSTIC_REPAIR_CLASS}"`)
    expect(html).toContain("casts/insert-explicit-conversion")
    expect(html).toContain("bindings/make-immutable")
    expect(html).toMatch(/aria-describedby="diagnostic-/)
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
