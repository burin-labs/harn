import { describe, expect, it } from "vitest"

import {
  AGENT_VERSION_NOTICE,
  SITE_ORIGIN,
  absoluteHtmlUrl,
  absoluteMarkdownUrl,
  agentPageFromDoc,
  clientPagePayload,
  renderAgentPage,
  renderLlmsFullTxt,
  renderLlmsTxt,
} from "./llms.mjs"

const gettingStarted = {
  slug: "getting-started",
  title: "Getting started",
  description: "Install Harn and run a first pipeline.",
  sectionTitle: "Tutorials",
  markdownSource: "# Getting started\n\nInstall the CLI.",
  prev: null,
  next: { title: "Introduction", slug: "introduction" },
}

const introduction = {
  slug: "introduction",
  title: "Introduction",
  description: "What Harn is and who it is for.",
  sectionTitle: "Explanation",
  markdownSource: "Harn is a language for agents.",
  prev: { title: "Getting started", slug: "getting-started" },
  next: null,
}

describe("agent page projection", () => {
  it("writes an agent-oriented page, not an HTML dump", () => {
    const md = renderAgentPage(gettingStarted)
    expect(md).toMatch(/^# Getting started\n/)
    expect(md).toContain("> Install Harn and run a first pipeline.")
    expect(md).toContain(`Website: ${absoluteHtmlUrl("getting-started")}`)
    expect(md).toContain(AGENT_VERSION_NOTICE)
    expect(md).toContain("Install the CLI.")
    expect(md.match(/^# Getting started$/gm)).toHaveLength(1)
    expect(md).toContain(`## Read next`)
    expect(md).toContain(`[Introduction](${absoluteMarkdownUrl("introduction")})`)
    expect(md).not.toContain("<h1>")
    expect(md).not.toContain("<html")
  })

  it("keeps a page with no neighbors from inventing a Read next list", () => {
    const md = renderAgentPage({
      ...gettingStarted,
      next: null,
    })
    expect(md).not.toContain("## Read next")
  })
})

describe("llms.txt", () => {
  it("indexes every page by section and points at .md URLs", () => {
    const txt = renderLlmsTxt([gettingStarted, introduction])
    expect(txt).toMatch(/^# Harn\n/)
    expect(txt).toContain(`Website: ${SITE_ORIGIN}/`)
    expect(txt).toContain("pre-1.0")
    expect(txt).toContain("### Tutorials")
    expect(txt).toContain("### Explanation")
    expect(txt.indexOf("### Tutorials")).toBeLessThan(txt.indexOf("### Explanation"))
    expect(txt).toContain(
      `[Getting started](${absoluteMarkdownUrl("getting-started")}): Install Harn and run a first pipeline.`,
    )
    expect(txt).toContain(`[Introduction](${absoluteMarkdownUrl("introduction")})`)
    expect(txt).toContain(`${SITE_ORIGIN}/llms-full.txt`)
    expect(txt).toContain(`${SITE_ORIGIN}/docs/llm/harn-quickref.md`)
  })

  it("concatenates every page into llms-full.txt", () => {
    const full = renderLlmsFullTxt([gettingStarted, introduction])
    expect(full).toContain(renderAgentPage(gettingStarted).trim())
    expect(full).toContain(renderAgentPage(introduction).trim())
    expect(full).toContain(AGENT_VERSION_NOTICE)
  })
})

describe("prerender payload", () => {
  it("strips markdownSource from the client JSON shape", () => {
    const page = {
      slug: "getting-started",
      url: "/getting-started.html",
      title: "Getting started",
      description: "Install Harn and run a first pipeline.",
      navTitle: "Getting started",
      sectionId: "tutorials",
      sectionTitle: "Tutorials",
      sourceRel: "getting-started.md",
      editUrl: "https://github.com/burin-labs/harn/edit/main/docs/src/getting-started.md",
      html: "<h1>Getting started</h1>",
      headings: [],
      prev: null,
      next: { title: "Introduction", url: "/introduction.html" },
      markdownSource: "secret-to-agents-only",
    }
    const payload = clientPagePayload(page)
    expect(payload.markdownSource).toBeUndefined()
    expect(payload.html).toBe(page.html)
    expect(agentPageFromDoc(page).next).toEqual({
      title: "Introduction",
      slug: "introduction",
    })
  })
})
