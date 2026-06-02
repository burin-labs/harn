import { defineConfig } from "vite"
import react from "@vitejs/plugin-react"
import tailwindcss from "@tailwindcss/vite"
import { harnDocsPlugin } from "./vite-plugins/harn-docs-plugin"

export default defineConfig({
  plugins: [react(), tailwindcss(), harnDocsPlugin()],
  build: {
    // Emit the static site into docs/dist — the directory Render publishes for
    // harnlang.com, and where build_docs_site.sh adds the raw .md mirror.
    outDir: "../docs/dist",
    emptyOutDir: true,
  },
})
