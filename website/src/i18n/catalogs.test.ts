import { describe, expect, it } from "vitest"
import { catalogs, format } from "./index"
import { resolveLocale } from "../../../ui/localization"

function flatten(value: unknown, prefix = "", result: Record<string, string> = {}): Record<string, string> {
  if (typeof value === "string") result[prefix] = value
  else if (value && typeof value === "object") {
    for (const [key, child] of Object.entries(value)) flatten(child, `${prefix}.${key}`, result)
  }
  return result
}

describe("website languages", () => {
  const source = flatten(catalogs.en)
  it.each(Object.entries(catalogs))("%s covers all copy and preserves interpolation", (_, catalog) => {
    const messages = flatten(catalog)
    expect(Object.keys(messages).sort()).toEqual(Object.keys(source).sort())
    for (const [key, english] of Object.entries(source)) {
      expect(messages[key].trim(), key).not.toBe("")
      expect(messages[key].match(/\{\w+\}/g)?.sort() ?? [], key).toEqual(english.match(/\{\w+\}/g)?.sort() ?? [])
    }
    expect(format(catalog.footer.copyright, { year: 2026 })).toContain("2026")
  })
  it("resolves browser region and script tags without claiming Traditional Chinese", () => {
    expect(resolveLocale("es-MX")).toBe("es")
    expect(resolveLocale("ru-RU")).toBe("ru")
    expect(resolveLocale("pt_BR")).toBe("pt-BR")
    expect(resolveLocale("zh-CN")).toBe("zh-Hans")
    expect(resolveLocale("zh-Hant-TW")).toBeUndefined()
    expect(resolveLocale("zz")).toBeUndefined()
  })
})
