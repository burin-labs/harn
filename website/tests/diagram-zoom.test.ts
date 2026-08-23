import { describe, expect, it } from "vitest"

import { rescopeStyles, zoomIdFor } from "../src/lib/diagram-zoom"

// A Mermaid SVG carries its whole stylesheet inside itself, scoped to the SVG's
// own id. The zoom overlay shows a *copy* of that SVG, which must therefore get
// a new id — and the copied rules must follow it. Getting this wrong is silent:
// the overlay opens, the diagram is there, and it renders in browser defaults
// (black fills, unstyled labels) rather than failing.
const MERMAID_STYLE = `#mermaid-7{font-family:"Inter";fill:#0c1413;}
#mermaid-7 .node rect{fill:#edf4f2;stroke:#0d9488;}
#mermaid-7 .edgePath .path{stroke:#0d9488;}
#mermaid-7 .nodeLabel{color:#0c1413;}`

describe("diagram zoom", () => {
  it("repoints every scoped rule at the copy's id", () => {
    const rescoped = rescopeStyles(MERMAID_STYLE, "mermaid-7", zoomIdFor("mermaid-7"))

    expect(rescoped).not.toContain("#mermaid-7{")
    expect(rescoped).not.toContain("#mermaid-7 ")
    expect(rescoped).toContain("#mermaid-7-zoom{")
    expect(rescoped).toContain("#mermaid-7-zoom .node rect{fill:#edf4f2;stroke:#0d9488;}")
    // Every rule moves, not just the first.
    expect(rescoped.split("#mermaid-7-zoom").length - 1).toBe(4)
  })

  it("leaves declarations that merely look like the id alone", () => {
    // `fill:#0d9488` must not be mistaken for a selector, and an unrelated
    // diagram's id on the same page must not be rewritten.
    const css = "#mermaid-1 .node{fill:#0d9488;}#mermaid-10 .node{fill:#fff;}"
    const rescoped = rescopeStyles(css, "mermaid-1", zoomIdFor("mermaid-1"))

    expect(rescoped).toContain("fill:#0d9488;")
    expect(rescoped).toContain("#mermaid-1-zoom .node{")
    // `#mermaid-10` shares a prefix with `#mermaid-1`; a prefix match would
    // corrupt it into `#mermaid-1-zoom0`.
    expect(rescoped).toContain("#mermaid-10 .node{fill:#fff;}")
  })

  it("gives the copy an id that cannot collide with the original", () => {
    expect(zoomIdFor("mermaid-3")).not.toBe("mermaid-3")
  })
})
