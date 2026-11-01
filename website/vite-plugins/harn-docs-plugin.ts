import { fileURLToPath } from "node:url"
import { dirname, resolve } from "node:path"
import { searchForWorkspaceRoot, type Plugin } from "vite"
import { collectDocContent, loadAllDocs, type LoadedDocs } from "./content.ts"

// `virtual:harn-docs` ships only the lightweight nav tree + per-slug metadata to
// the client. Per-page rendered HTML and the search index are served as JSON
// (lazily fetched on navigation / first search) — in dev from this plugin's
// middleware, in production as static files written by the SSG prerender. This
// keeps the client bundle tiny regardless of how many docs pages exist.
const VIRTUAL_ID = "virtual:harn-docs"
const RESOLVED_ID = "\0" + VIRTUAL_ID

const here = dirname(fileURLToPath(import.meta.url))
export const REPO_ROOT = resolve(here, "../..")

export function harnDocsPlugin(): Plugin {
  const docsRoot = resolve(REPO_ROOT, "docs/src")
  let cache: LoadedDocs | null = null
  const get = () => (cache ??= loadAllDocs(REPO_ROOT))
  let assets: Map<string, string> | null = null
  const getAssets = () => (assets ??= new Map(
    collectDocContent(docsRoot).assets.map((asset) => ["/" + asset.sourceRel, asset.sourcePath]),
  ))

  return {
    name: "harn-docs",
    config(config) {
      // An explicit allow list replaces Vite's workspace default. Preserve
      // that default (or the caller's list), adding the shared site sources.
      const allow = config.server?.fs?.allow ?? [searchForWorkspaceRoot(resolve(config.root ?? process.cwd()))]
      return {
        server: {
          fs: {
            allow: [...allow, docsRoot, resolve(REPO_ROOT, "ui"), resolve(REPO_ROOT, "crates/harn-cli/assets/demo")],
          },
        },
      }
    },
    resolveId(id) {
      if (id === VIRTUAL_ID) return RESOLVED_ID
    },
    load(id) {
      if (id !== RESOLVED_ID) return
      const { nav, meta } = get()
      return `export const nav = ${JSON.stringify(nav)};\nexport const meta = ${JSON.stringify(meta)};\n`
    },
    configureServer(server) {
      // Invalidate when docs content or the generated keyword table changes.
      server.watcher.add(resolve(REPO_ROOT, "docs/src"))
      server.watcher.add(resolve(REPO_ROOT, "docs/theme/harn-keywords.js"))
      const invalidate = (file: string) => {
        if (file.includes("/docs/src/") || file.endsWith("harn-keywords.js")) {
          cache = null
          assets = null
          const mod = server.moduleGraph.getModuleById(RESOLVED_ID)
          if (mod) server.moduleGraph.invalidateModule(mod)
          server.ws.send({ type: "full-reload" })
        }
      }
      server.watcher.on("change", invalidate)
      server.watcher.on("add", invalidate)
      server.watcher.on("unlink", invalidate)

      server.middlewares.use((req, res, next) => {
        const url = (req.url ?? "").split("?")[0]
        if (url.startsWith("/_content/")) {
          const docs = get()
          if (url === "/_content/search.json") {
            res.setHeader("Content-Type", "application/json")
            res.end(JSON.stringify(docs.search))
            return
          }
          const slug = decodeURIComponent(url.slice("/_content/".length).replace(/\.json$/, ""))
          const page = docs.pages.get(slug)
          res.statusCode = page ? 200 : 404
          res.setHeader("Content-Type", "application/json")
          res.end(JSON.stringify(page ?? {}))
          return
        }
        if (req.method === "GET" || req.method === "HEAD") {
          let asset
          try {
            asset = getAssets().get(decodeURIComponent(url))
          } catch (error) {
            if (!(error instanceof URIError)) throw error
          }
          if (asset) {
            // Vite's native filesystem middleware owns bytes, MIME, cache,
            // and access checks. Only inventoried URLs receive this rewrite.
            const fileUrl = encodeURI(asset.replaceAll("\\", "/")).replace(/[?#]/g, encodeURIComponent)
            req.url = "/@fs/" + fileUrl
            next()
            return
          }
        }
        // SPA fallback: rewrite `.html` doc URLs to "/" so Vite serves the app
        // shell (it would otherwise 404 trying to resolve them as static files).
        const accepts = req.headers.accept ?? ""
        if (
          req.method === "GET" &&
          accepts.includes("text/html") &&
          url.endsWith(".html") &&
          !url.startsWith("/@")
        ) {
          req.url = "/"
        }
        next()
      })
    },
  }
}
