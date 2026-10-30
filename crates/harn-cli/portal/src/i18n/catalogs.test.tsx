import { act, fireEvent, render, screen } from "@testing-library/react"
import { MemoryRouter } from "react-router"
import { afterEach, describe, expect, it } from "vitest"
import ts from "typescript"
import { createIntl } from "react-intl"
import { Sidebar } from "../components/Sidebar"
import { getLocale, setLocale } from "../../../../../ui/localization"
import { PortalIntlProvider } from "./PortalIntlProvider"
import { catalogs } from "./catalogs"

const sources = import.meta.glob<string>(
  ["../**/*.{ts,tsx}", "!../**/*.test.*", "!../i18n/**"],
  {
    query: "?raw",
    import: "default",
    eager: true,
  },
)

afterEach(() => {
  act(() => setLocale("en"))
})

describe("portal languages", () => {
  it.each(Object.entries(catalogs))(
    "%s has a complete, parseable catalog",
    (_, catalog) => {
      expect(Object.keys(catalog).sort()).toEqual(
        Object.keys(catalogs.en).sort(),
      )
      for (const [id, message] of Object.entries(catalog)) {
        expect(message.trim(), id).not.toBe("")
        const values = Object.fromEntries(
          [...message.matchAll(/\{(\w+)[},]/g)].map((match) => [match[1], 2]),
        )
        const intl = createIntl({
          locale: "en",
          messages: catalog,
          onError: (error) => {
            throw error
          },
        })
        expect(() => intl.formatMessage({ id }, values), id).not.toThrow()
        expect(message.match(/\{\w+\}/g)?.sort() ?? [], id).toEqual(
          catalogs.en[id as keyof typeof catalogs.en]
            .match(/\{\w+\}/g)
            ?.sort() ?? [],
        )
      }
    },
  )

  it("covers the descriptors and translated literal messages used by the portal", () => {
    const found = new Map<string, string>()
    for (const [file, content] of Object.entries(sources)) {
      const source = ts.createSourceFile(
        file,
        content,
        ts.ScriptTarget.Latest,
        true,
        file.endsWith(".tsx") ? ts.ScriptKind.TSX : ts.ScriptKind.TS,
      )
      function visit(node: ts.Node): void {
        if (
          ts.isCallExpression(node) &&
          node.expression.getText(source) === "portalText" &&
          node.arguments[0] &&
          ts.isStringLiteral(node.arguments[0])
        ) {
          found.set(node.arguments[0].text, node.arguments[0].text)
        }
        if (ts.isObjectLiteralExpression(node)) {
          const properties = new Map(
            node.properties
              .filter(ts.isPropertyAssignment)
              .map((p) => [p.name.getText(source), p.initializer]),
          )
          const id = properties.get("id")
          const message = properties.get("defaultMessage")
          if (
            id &&
            message &&
            ts.isStringLiteralLike(id) &&
            ts.isStringLiteralLike(message)
          ) {
            found.set(id.text, message.text)
          }
        }
        ts.forEachChild(node, visit)
      }
      visit(source)
    }
    expect(found.size).toBeGreaterThan(300)
    expect(Object.fromEntries(found)).toEqual(catalogs.en)
  })

  it("switches the real sidebar without replacing the app and persists the language", async () => {
    localStorage.setItem("harn.language", "en")
    render(
      <MemoryRouter>
        <PortalIntlProvider>
          <Sidebar
            stats={null}
            loading={false}
            lastRefreshAt={null}
            lastError={null}
            onRefresh={() => {}}
          />
        </PortalIntlProvider>
      </MemoryRouter>,
    )
    const selector = screen.getByRole("combobox")
    fireEvent.change(selector, { target: { value: "fr" } })
    expect(
      await screen.findByRole("link", {
        name: catalogs.fr["portal.sidebar.runs"],
      }),
    ).toBeInTheDocument()
    expect(selector).toBe(screen.getByRole("combobox"))
    expect(getLocale()).toBe("fr")
    expect(document.documentElement.lang).toBe("fr")
    expect(localStorage.getItem("harn.language")).toBe("fr")
    fireEvent.change(selector, { target: { value: "ja" } })
    expect(
      await screen.findByRole("link", {
        name: catalogs.ja["portal.sidebar.runs"],
      }),
    ).toBeInTheDocument()
  })
})
