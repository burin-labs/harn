import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"
import { afterAll, beforeAll, describe, expect, it } from "vitest"
import { createServer, searchForWorkspaceRoot, type ViteDevServer } from "vite"
import { harnDocsPlugin, REPO_ROOT } from "./harn-docs-plugin.ts"

const websiteRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..")
let server: ViteDevServer
let origin: string
let outside: string

beforeAll(async () => {
  outside = mkdtempSync(join(tmpdir(), "harn-docs-dev-denied-"))
  writeFileSync(join(outside, "private.json"), '{"outside":"not documentation"}')
  server = await createServer({
    configFile: false,
    root: websiteRoot,
    plugins: [harnDocsPlugin()],
    optimizeDeps: { noDiscovery: true },
    server: { host: "127.0.0.1", port: 0, hmr: false },
    logLevel: "silent",
  })
  await server.listen()
  const address = server.httpServer!.address()
  if (!address || typeof address === "string") throw new Error("dev server did not bind a TCP port")
  origin = `http://127.0.0.1:${address.port}`
})

afterAll(async () => {
  await server?.close()
  if (outside) rmSync(outside, { recursive: true, force: true })
})

describe("documentation assets in the real Vite dev server", () => {
  it.each([
    ["orchestrator/observability-dashboard.json", "application/json"],
    ["cookbooks/assets/model-job-openai-proof.png", "image/png"],
    ["orchestrator/assets/dlq-management.svg", "image/svg+xml"],
  ])("serves %s at its published URL with native MIME and exact bytes", async (path, mime) => {
    const response = await fetch(`${origin}/${path}`, { headers: { Accept: mime } })
    expect(response.status).toBe(200)
    expect(response.headers.get("content-type")).toContain(mime)
    const bytes = Buffer.from(await response.arrayBuffer())
    expect(bytes.equals(readFileSync(join(REPO_ROOT, "docs/src", path)))).toBe(true)
    if (mime === "application/json") {
      expect(JSON.parse(bytes.toString("utf8")).panels.length).toBeGreaterThan(0)
    }
  })

  it("uses Vite's conditional GET cache handling", async () => {
    const url = `${origin}/orchestrator/observability-dashboard.json`
    const first = await fetch(url, { headers: { Accept: "application/json" } })
    expect(first.status).toBe(200)
    await first.arrayBuffer()
    const etag = first.headers.get("etag")
    expect(etag).toBeTruthy()
    const cached = await fetch(url, { headers: { Accept: "application/json", "If-None-Match": etag! } })
    expect(cached.status).toBe(304)
  })

  it("preserves the default allowed workspace and adds only the docs source tree", () => {
    expect(server.config.server.fs.allow).toContain(searchForWorkspaceRoot(websiteRoot))
    expect(server.config.server.fs.allow).toContain(join(REPO_ROOT, "docs/src"))
    expect(server.config.server.fs.allow).not.toContain(REPO_ROOT)
    expect(server.config.server.fs.allow).not.toContain(outside)
  })

  it("does not turn unknown documentation URLs into filesystem access", async () => {
    const missing = await fetch(`${origin}/dev/perf/missing-evidence.json`, { headers: { Accept: "application/json" } })
    expect(missing.status).toBe(404)
    const denied = await fetch(`${origin}/@fs/${join(outside, "private.json")}`, { headers: { Accept: "application/json" } })
    expect(denied.status).toBe(403)
    expect(await denied.text()).not.toContain('"outside":"not documentation"')
  })
})
