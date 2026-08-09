import { renderToStaticMarkup } from "react-dom/server"
import { describe, expect, it } from "vitest"
import { ExampleGallery, exampleScenarios } from "../src/components/ExampleGallery"

describe("ExampleGallery", () => {
  it("renders the curated runnable example tabs, leading with a coding agent", () => {
    const markup = renderToStaticMarkup(<ExampleGallery />)
    const scenarioTabs = (markup.match(/data-scenario-tab="true"/g) ?? []).length

    expect(scenarioTabs).toBe(4)
    expect(exampleScenarios.map((scenario) => scenario.slug)).toMatchInlineSnapshot(`
      [
        "review-captain",
        "merge-captain",
        "mcp-host",
        "stdlib-toolkit",
      ]
    `)
  })

  it("leads each scenario's primary file with code, not the design-note header comment", () => {
    for (const scenario of exampleScenarios) {
      const primary = scenario.files[0]
      expect(primary.name).toBe("scenario.harn")
      expect(primary.source.startsWith("/*")).toBe(false)
    }
  })

  it("surfaces review-captain's sibling .harn.prompt templates as switchable files", () => {
    const reviewCaptain = exampleScenarios.find((s) => s.slug === "review-captain")
    expect(reviewCaptain?.files.map((f) => f.name)).toEqual([
      "scenario.harn",
      "review.harn.prompt",
      "clarification.harn.prompt",
    ])
    const promptFiles = reviewCaptain?.files.filter((f) => f.lang === "prompt") ?? []
    expect(promptFiles).toHaveLength(2)
    // The prompt templates carry the render_prompt interpolation markers.
    for (const file of promptFiles) {
      expect(file.source).toMatch(/\{\{.*\}\}/)
    }
  })

  it("renders the file switcher only for multi-file scenarios", () => {
    const markup = renderToStaticMarkup(<ExampleGallery />)
    // review-captain is first and is multi-file, so file tabs render initially.
    const fileTabs = (markup.match(/data-file-tab="true"/g) ?? []).length
    expect(fileTabs).toBe(3)
  })
})
