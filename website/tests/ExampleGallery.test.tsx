import { renderToStaticMarkup } from "react-dom/server"
import { describe, expect, it } from "vitest"
import { ExampleGallery, exampleScenarios } from "../src/components/ExampleGallery"

describe("ExampleGallery", () => {
  it("renders the curated runnable example tabs, leading with a coding agent", () => {
    const markup = renderToStaticMarkup(<ExampleGallery />)
    const tabCount = (markup.match(/role="tab"/g) ?? []).length

    expect(tabCount).toBe(4)
    expect(exampleScenarios.map((scenario) => scenario.slug)).toMatchInlineSnapshot(`
      [
        "review-captain",
        "merge-captain",
        "mcp-host",
        "stdlib-toolkit",
      ]
    `)
  })

  it("leads each scenario with code, not the design-note header comment", () => {
    for (const scenario of exampleScenarios) {
      expect(scenario.source.startsWith("/*")).toBe(false)
    }
  })
})
