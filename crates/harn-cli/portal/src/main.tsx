import React from "react"
import ReactDOM from "react-dom/client"
import { QueryClientProvider } from "@tanstack/react-query"
import { IntlProvider } from "react-intl"
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
        <IntlProvider locale="en">
          <App />
        </IntlProvider>
      </BrowserRouter>
    </QueryClientProvider>
  </React.StrictMode>,
)
