import { renderToStaticMarkup } from "react-dom/server"
import { describe, expect, it } from "vitest"
import { HarnMockup, heroSnippetSource } from "../src/components/HarnMockup"
import heroSnippetRaw from "../src/examples/hero.harn.txt?raw"

function decodeEntities(value: string) {
  return value
    .replaceAll("&amp;", "&")
    .replaceAll("&lt;", "<")
    .replaceAll("&gt;", ">")
    .replaceAll("&quot;", '"')
    .replaceAll("&#x27;", "'")
    .replaceAll("&#39;", "'")
}

function heroSnippetText(markup: string) {
  const match = markup.match(/<code[^>]*data-hero-snippet="true"[^>]*>([\s\S]*?)<\/code>/)
  if (!match) throw new Error("hero snippet code block was not rendered")
  return decodeEntities(match[1].replace(/<[^>]*>/g, ""))
}

describe("HarnMockup", () => {
  it("renders the checked-in hero snippet source", () => {
    expect(heroSnippetSource).toBe(heroSnippetRaw.trimEnd())
    expect(heroSnippetText(renderToStaticMarkup(<HarnMockup />))).toBe(heroSnippetSource)
  })
})
