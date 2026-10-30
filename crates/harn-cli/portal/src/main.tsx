import React from "react"
import ReactDOM from "react-dom/client"
import { QueryClientProvider } from "@tanstack/react-query"
import { PortalIntlProvider } from "./i18n/PortalIntlProvider"
import { BrowserRouter } from "react-router"

import { App } from "./App"
import { createQueryClient } from "./lib/query"
import "./styles.css"

const rootElement = document.getElementById("root")
if (!rootElement) {
  throw new Error("portal: missing #root mount point in index.html")
}
const queryClient = createQueryClient()
ReactDOM.createRoot(rootElement).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>
      <BrowserRouter>
        <PortalIntlProvider>
          <App />
        </PortalIntlProvider>
      </BrowserRouter>
    </QueryClientProvider>
  </React.StrictMode>,
)
