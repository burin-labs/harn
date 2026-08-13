import { dirname, resolve } from "node:path"
import { fileURLToPath } from "node:url"
import { describe, expect, it } from "vitest"

import { loadAllDocs } from "./content.ts"

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..")

describe("documentation content contract", () => {
  it(
    "indexes every page and resolves every internal page and anchor link",
    () => {
      expect(() => loadAllDocs(repoRoot)).not.toThrow()
    },
    60_000,
  )
})
