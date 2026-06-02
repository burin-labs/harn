import { StrictMode } from "react"
import { hydrateRoot, createRoot } from "react-dom/client"
import { createBrowserRouter, RouterProvider } from "react-router"
import { routes } from "./routes"
import { seedPage } from "./lib/page-store"
import "./index.css"

// Seed the page cache from the SSR-inlined payload so the first client render
// matches the prerendered HTML and hydrates cleanly.
const payloadEl = document.getElementById("__HARN_PAGE__")
if (payloadEl?.textContent) {
  try {
    seedPage(JSON.parse(payloadEl.textContent))
  } catch {
    /* ignore malformed payload */
  }
}

const router = createBrowserRouter(routes)
const root = document.getElementById("root")!

const app = (
  <StrictMode>
    <RouterProvider router={router} />
  </StrictMode>
)

// Prerendered pages have server markup to hydrate; a cold dev load does not.
if (root.childNodes.length > 0) {
  hydrateRoot(root, app)
} else {
  createRoot(root).render(app)
}
