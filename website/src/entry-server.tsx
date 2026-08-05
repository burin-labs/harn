import { StrictMode } from "react"
import { renderToString } from "react-dom/server"
import { createMemoryRouter, RouterProvider } from "react-router"
import { routes } from "./routes"
import { seedPage } from "./lib/page-store"
import type { PageData } from "../vite-plugins/content"

export { loadAllDocs } from "../vite-plugins/content"
export { REPO_ROOT } from "../vite-plugins/harn-docs-plugin"
export { LANDING_PAGE_META, NOT_FOUND_PAGE_META, pageMetaForDoc } from "./lib/metadata"

// Render one route to an HTML string. `pageData` (when supplied) is seeded into
// the page cache so doc pages render their content synchronously.
export function render(url: string, pageData?: PageData | null): string {
  if (pageData) seedPage(pageData)
  const router = createMemoryRouter(routes, { initialEntries: [url] })
  return renderToString(
    <StrictMode>
      <RouterProvider router={router} />
    </StrictMode>,
  )
}
