import { defineConfig } from "vite"
import react from "@vitejs/plugin-react"
import tailwindcss from "@tailwindcss/vite"
import { harnDocsPlugin } from "./vite-plugins/harn-docs-plugin.ts"

export default defineConfig({
  plugins: [react(), tailwindcss(), harnDocsPlugin()],
  build: {
    // Emit the static site into docs/dist — the directory Render publishes for
    // harnlang.com. prerender.mjs also writes llms.txt and per-page .md.
    outDir: "../docs/dist",
    emptyOutDir: true,
  },
})
