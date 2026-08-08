import { defineConfig } from "vite"
import react from "@vitejs/plugin-react"

export default defineConfig({
  plugins: [react()],
  server: {
    host: "127.0.0.1",
    port: 4723,
    proxy: {
      "/api": "http://127.0.0.1:4721",
    },
  },
  build: {
    outDir: "../portal-dist",
    emptyOutDir: true,
    cssCodeSplit: false,
    rollupOptions: {
      output: {
        entryFileNames: "assets/portal/app.js",
        chunkFileNames: "assets/portal/[name].js",
        assetFileNames: (assetInfo) => {
          if (assetInfo.name?.endsWith(".css")) {
            return "assets/portal/styles.css"
          }
          return "assets/portal/[name][extname]"
        },
      },
    },
  },
  test: {
    environment: "jsdom",
    setupFiles: "./src/test/setup.ts",
  },
})
