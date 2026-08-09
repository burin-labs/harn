import React from "react"
import ReactDOM from "react-dom/client"
import { IntlProvider } from "react-intl"
import { BrowserRouter } from "react-router"

import { App } from "./App"
import "./styles.css"

const rootElement = document.getElementById("root")
if (!rootElement) {
  throw new Error("portal: missing #root mount point in index.html")
}
ReactDOM.createRoot(rootElement).render(
  <React.StrictMode>
    <BrowserRouter>
      <IntlProvider locale="en">
        <App />
      </IntlProvider>
    </BrowserRouter>
  </React.StrictMode>,
)
