import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { afterEach, describe, expect, it } from "vitest"
import { resolveIncludes } from "./content.ts"

const roots: string[] = []
function fixture(source: string) {
  const root = mkdtempSync(join(tmpdir(), "harn-doc-includes-"))
  roots.push(root)
  writeFileSync(join(root, "recipe.harn"), source)
  return (spec: string) => resolveIncludes(`{{#include ${spec}}}`, join(root, "page.md"), root)
}
afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true })
})

describe("executable-source documentation includes", () => {
  it("projects exactly the named constructor region", () => {
    const include = fixture("setup\n// ANCHOR: stack\nconst caller = compose(clock, [])\n// ANCHOR_END: stack\nassert(caller)\n")
    expect(include("recipe.harn:stack")).toBe("const caller = compose(clock, [])")
    expect(include("\nrecipe.harn\n:stack\n")).toBe("const caller = compose(clock, [])")
  })

  it("preserves whole-file and inclusive numeric ranges", () => {
    const source = "first\nsecond\nthird\nfourth"
    const include = fixture(source)
    expect(include("recipe.harn")).toBe(source)
    expect(include("recipe.harn:2:3")).toBe("second\nthird")
    expect(include("recipe.harn:3")).toBe("third\nfourth")
    expect(include("recipe.harn::2")).toBe("first\nsecond")
  })

  it.each([
    ["const caller = compose(clock, [])", "0 starts, 0 ends"],
    ["// ANCHOR: stack\nbody", "1 starts, 0 ends"],
    ["// ANCHOR_END: stack\nbody\n// ANCHOR: stack", "1 starts, 1 ends"],
    ["// ANCHOR: stack\na\n// ANCHOR_END: stack\n// ANCHOR: stack\nb\n// ANCHOR_END: stack", "2 starts, 2 ends"],
  ])("refuses an absent, incomplete, reversed, or duplicate region", (source, counts) => {
    expect(() => fixture(source)("recipe.harn:stack")).toThrow(counts)
  })

  it("refuses a missing file instead of projecting an empty named region", () => {
    expect(() => fixture("")("missing.harn:stack")).toThrow("missing named include")
  })

  it.each(["", " \n\t"])("refuses an empty or whitespace-only region", (body) => {
    const include = fixture(`// ANCHOR: stack\n${body}\n// ANCHOR_END: stack`)
    expect(() => include("recipe.harn:stack")).toThrow("empty named include")
  })
})
