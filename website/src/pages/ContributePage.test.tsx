// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react"
import { createMemoryRouter, RouterProvider } from "react-router"
import { routes } from "../routes"
import { catalogs, format, type Locale } from "../i18n"
import { setLocale } from "../../../ui/localization"

beforeEach(() => {
  localStorage.clear()
  setLocale("en")
  vi.spyOn(window, "scrollTo").mockImplementation(() => {})
})

afterEach(() => {
  cleanup()
  vi.restoreAllMocks()
})

function openPage(path: string): ReturnType<typeof render> {
  return render(<RouterProvider router={createMemoryRouter(routes, { initialEntries: [path] })} />)
}

describe("localized contribution route", () => {
  it.each(Object.keys(catalogs) as Locale[])("switches the mounted page to %s", (locale) => {
    openPage("/contribute.html")
    const heading = screen.getByRole("heading", { level: 1 })
    expect(heading.textContent).toBe("Help improve Harn")
    fireEvent.change(screen.getByRole("combobox"), { target: { value: locale } })
    const t = catalogs[locale].contribute
    expect(screen.getByRole("heading", { level: 1 })).toBe(heading)
    expect(heading.textContent).toBe(t.title)
    expect(document.documentElement.lang).toBe(locale)
    expect(document.title).toBe(format(t.pageTitle, { siteName: "Harn" }))
    expect(document.querySelector('meta[name="description"]')?.getAttribute("content")).toBe(t.intro)
    const article = heading.closest("article")!
    const links = within(article).getAllByRole("link")
    expect(links.map((link) => link.getAttribute("href"))).toEqual([
      "https://github.com/burin-labs/harn/issues",
      "https://github.com/burin-labs/harn/blob/main/CONTRIBUTING.md",
      "https://github.com/burin-labs/harn/discussions",
      "https://buy.stripe.com/fZubIUaPo6gv2hu5d34c800",
    ])
    expect(links.map((link) => link.textContent)).toEqual([t.reportLink, t.codeLink, t.discussLink, t.tipLink])
    expect(article.textContent).toContain(format(t.tipBody, { company: "Burin Labs, LLC" }))
    for (const link of links) expect(link.getAttribute("rel")).toBe("noopener noreferrer")
  })

  it("reaches the contribution page from the existing community footer", async () => {
    openPage("/missing-page.html")
    fireEvent.click(screen.getByRole("link", { name: catalogs.en.footer.contributing }))
    await screen.findByRole("heading", { level: 1, name: catalogs.en.contribute.title })
    expect(document.title).toBe(format(catalogs.en.contribute.pageTitle, { siteName: "Harn" }))
  })
})

describe("existing page metadata follows the language control", () => {
  it.each(["/", "/missing-page.html"])("updates the title and description on %s", (path) => {
    openPage(path)
    const expected = path === "/" ? catalogs.fr.meta.landingTitle : format(catalogs.fr.meta.notFoundTitle, { siteName: "Harn" })
    expect(document.title).not.toBe(expected)
    fireEvent.change(screen.getByRole("combobox"), { target: { value: "fr" } })
    expect(document.documentElement.lang).toBe("fr")
    expect(document.title).toBe(expected)
    expect(document.querySelector('meta[name="description"]')?.getAttribute("content")).toBe(
      path === "/" ? catalogs.fr.meta.landingDescription : catalogs.fr.meta.notFoundDescription,
    )
  })
})
