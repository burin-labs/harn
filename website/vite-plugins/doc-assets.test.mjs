import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync, existsSync, symlinkSync } from "node:fs"
import { tmpdir } from "node:os"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"
import { afterEach, describe, expect, it } from "vitest"
import { collectDocContent } from "./content.ts"
import { publishDocAssets } from "./doc-assets.mjs"

const roots = []
function fixture() {
  const root = mkdtempSync(join(tmpdir(), "harn-doc-assets-"))
  roots.push(root)
  const source = join(root, "src")
  const dist = join(root, "dist")
  mkdirSync(source)
  mkdirSync(dist)
  const writeSource = (path, data) => {
    const target = join(source, path)
    mkdirSync(dirname(target), { recursive: true })
    writeFileSync(target, data)
  }
  return { source, dist, writeSource }
}

afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true })
})

describe("documentation asset publication", () => {
  it("publishes the discovered JSON and binary bytes at source-relative URLs", () => {
    const { source, dist, writeSource } = fixture()
    const evidence = JSON.stringify({ trials: [{ elapsed_us: 4156750, accepted: true }] })
    const image = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0, 0xff, 0x80])
    writeSource("SUMMARY.md", "# Summary\n")
    writeSource("dev/report.md", "# Report\n[Trials](perf/trials.json)\n![Proof](assets/proof.png)\n")
    writeSource("dev/perf/trials.json", evidence)
    writeSource("dev/assets/proof.png", image)
    const content = collectDocContent(source)
    expect(content.pages).toEqual([join(source, "dev/report.md")])
    expect(content.assets.map((asset) => asset.sourceRel).sort()).toEqual([
      "dev/assets/proof.png", "dev/perf/trials.json",
    ])
    // These are the neighboring outputs produced before assets by prerender.
    mkdirSync(join(dist, "dev"))
    writeFileSync(join(dist, "dev/report.html"), '<a href="/dev/perf/trials.json">Trials</a>')
    writeFileSync(join(dist, "dev/report.md"), readFileSync(content.pages[0]))
    publishDocAssets(dist, content.assets)
    expect(readFileSync(join(dist, "dev/perf/trials.json"), "utf8")).toBe(evidence)
    expect(JSON.parse(readFileSync(join(dist, "dev/perf/trials.json"), "utf8")).trials).toHaveLength(1)
    const publishedImage = readFileSync(join(dist, "dev/assets/proof.png"))
    expect(publishedImage.equals(image)).toBe(true)
    const alteredImage = Buffer.from(image)
    alteredImage[alteredImage.length - 1] ^= 1
    expect(publishedImage.equals(alteredImage)).toBe(false)
    expect(readFileSync(join(dist, "dev/report.md"), "utf8")).toContain("perf/trials.json")
  })

  it.each(["index.html", "report.md", "_content/search.json", "assets/site.svg"])(
    "rejects a collision with %s before publishing any assets",
    (path) => {
      const { source, dist, writeSource } = fixture()
      writeSource("evidence.json", '{"trials":[1]}')
      writeSource(path, "source collision")
      const target = join(dist, path)
      mkdirSync(dirname(target), { recursive: true })
      writeFileSync(target, "site-owned")
      const collision = { sourceRel: path, sourcePath: join(source, path) }
      const assets = collectDocContent(source).assets.filter((asset) => asset.sourceRel !== path)
      expect(() => publishDocAssets(dist, [...assets, collision])).toThrow("collides with site output")
      expect(readFileSync(target, "utf8")).toBe("site-owned")
      expect(existsSync(join(dist, "evidence.json"))).toBe(false)
    },
  )

  it("rejects an asset below a generated file or outside the output root", () => {
    const { source, dist, writeSource } = fixture()
    writeSource("dev/evidence.json", '{"trials":[1]}')
    writeFileSync(join(dist, "dev"), "site-owned")
    const content = collectDocContent(source)
    expect(() => publishDocAssets(dist, content.assets)).toThrow("collides with site output")
    expect(() => publishDocAssets(dist, [{ ...content.assets[0], sourceRel: "../escape.json" }]))
      .toThrow("invalid output path")
    expect(existsSync(join(dirname(dist), "escape.json"))).toBe(false)
  })

  it("refuses a symlink output parent before writing outside the site", () => {
    const { source, dist, writeSource } = fixture()
    writeSource("dev/evidence.json", '{"trials":[1]}')
    const outside = join(dirname(dist), "outside")
    mkdirSync(outside)
    writeFileSync(join(outside, "owned.txt"), "external bytes")
    symlinkSync(outside, join(dist, "dev"), "dir")
    let error
    try {
      publishDocAssets(dist, collectDocContent(source).assets)
    } catch (caught) {
      error = caught
    }
    expect(existsSync(join(outside, "evidence.json"))).toBe(false)
    expect(readFileSync(join(outside, "owned.txt"), "utf8")).toBe("external bytes")
    expect(error?.message).toContain("collides with site output")
  })

  it("refuses a symlink output root and a dangling symlink target", () => {
    const { source, dist, writeSource } = fixture()
    writeSource("evidence.json", '{"trials":[1]}')
    const content = collectDocContent(source)
    const alias = join(dirname(dist), "dist-link")
    symlinkSync(dist, alias, "dir")
    expect(() => publishDocAssets(alias, content.assets)).toThrow("directory owned by the site")
    expect(existsSync(join(dist, "evidence.json"))).toBe(false)
    const outside = join(dirname(dist), "missing.json")
    symlinkSync(outside, join(dist, "evidence.json"))
    expect(() => publishDocAssets(dist, content.assets)).toThrow("collides with site output")
    expect(existsSync(outside)).toBe(false)
  })

  it.each(["file", "directory", "cycle", "root"])(
    "refuses a source %s symlink without traversing or publishing it",
    (kind) => {
      const { source, dist, writeSource } = fixture()
      writeSource("report.md", "# Report\n")
      const outside = join(dirname(dist), "outside")
      mkdirSync(outside)
      const externalFile = join(outside, "evidence.json")
      writeFileSync(externalFile, '{"external":true}')
      let inventoryRoot = source
      if (kind === "root") {
        inventoryRoot = join(dirname(source), "source-link")
        symlinkSync(source, inventoryRoot, "dir")
      } else {
        const target = kind === "file" ? externalFile : kind === "cycle" ? source : outside
        symlinkSync(target, join(source, "linked-content"), kind === "file" ? "file" : "dir")
      }
      expect(() => collectDocContent(inventoryRoot)).toThrow("cannot be a symbolic link")
      expect(existsSync(join(dist, "linked-content"))).toBe(false)
      expect(readFileSync(externalFile, "utf8")).toBe('{"external":true}')
    },
  )

  it("publishes every canonical source asset, including nonempty JSON", () => {
    const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..")
    const content = collectDocContent(join(repoRoot, "docs/src"))
    const { dist } = fixture()
    expect(content.assets.length).toBeGreaterThan(0)
    publishDocAssets(dist, content.assets)
    for (const asset of content.assets) {
      // Native byte comparison avoids recursively inspecting large Buffers.
      const published = readFileSync(join(dist, asset.sourceRel))
      expect(published.equals(readFileSync(asset.sourcePath)), asset.sourceRel).toBe(true)
    }
    const dashboard = JSON.parse(readFileSync(join(dist, "orchestrator/observability-dashboard.json"), "utf8"))
    expect(dashboard.panels.length).toBeGreaterThan(0)
  })
})
