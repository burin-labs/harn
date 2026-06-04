import { renderToStaticMarkup } from "react-dom/server"
import { describe, expect, it } from "vitest"
import { ExampleGallery, exampleScenarios } from "../src/components/ExampleGallery"

describe("ExampleGallery", () => {
  it("renders the curated runnable example tabs", () => {
    const markup = renderToStaticMarkup(<ExampleGallery />)
    const tabCount = (markup.match(/role="tab"/g) ?? []).length

    expect(tabCount).toBe(3)
    expect(exampleScenarios.map((scenario) => scenario.slug)).toMatchInlineSnapshot(`
      [
        "mcp-host",
        "merge-captain",
        "stdlib-toolkit",
      ]
    `)
  })
})
