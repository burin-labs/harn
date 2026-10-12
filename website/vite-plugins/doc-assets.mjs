import { constants, copyFileSync, lstatSync, mkdirSync } from "node:fs"
import { dirname, join, posix, resolve } from "node:path"

// The content inventory owns asset discovery. The static publisher only
// validates output ownership and preserves the source-relative URL and bytes.
export function publishDocAssets(dist, assets) {
  dist = resolve(dist)
  const root = lstatSync(dist)
  if (!root.isDirectory() || root.isSymbolicLink()) {
    throw new Error("documentation assets require a directory owned by the site")
  }
  const targets = new Set()
  for (const { sourceRel } of assets) {
    if (!sourceRel || sourceRel.includes("\\") || posix.isAbsolute(sourceRel) ||
        posix.normalize(sourceRel) !== sourceRel || sourceRel.startsWith("../")) {
      throw new Error(`documentation asset has an invalid output path: ${sourceRel}`)
    }
    const target = join(dist, sourceRel)
    let parent = dirname(target)
    while (parent !== dist) {
      const entry = lstatSync(parent, { throwIfNoEntry: false })
      if (entry && (!entry.isDirectory() || entry.isSymbolicLink())) {
        throw new Error(`documentation asset collides with site output: ${sourceRel}`)
      }
      parent = dirname(parent)
    }
    if (targets.has(target) || lstatSync(target, { throwIfNoEntry: false })) {
      throw new Error(`documentation asset collides with site output: ${sourceRel}`)
    }
    targets.add(target)
  }
  for (const { sourcePath, sourceRel } of assets) {
    const target = join(dist, sourceRel)
    mkdirSync(dirname(target), { recursive: true })
    copyFileSync(sourcePath, target, constants.COPYFILE_EXCL)
  }
}
