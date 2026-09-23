import { appendFileSync, existsSync, readFileSync } from 'node:fs'
import { basename, dirname, isAbsolute, join, relative, resolve } from 'node:path'

const [rawPath, rootPath, floorText] = process.argv.slice(2)
if (!rawPath || !rootPath || !/^\d+$/.test(floorText ?? '')) {
  throw new Error('usage: collect_stack_sources.mjs RAW_JSON REPO_ROOT FLOOR_BYTES')
}

const root = resolve(rootPath)
const covered = new Set()
let artifactDepfiles = 0
for (const line of readFileSync(rawPath, 'utf8').split('\n')) {
  let record
  try {
    record = JSON.parse(line)
  } catch {
    continue
  }
  if (record?.reason !== 'compiler-artifact' || !Array.isArray(record.filenames)) continue
  for (const filename of record.filenames) {
    if (typeof filename !== 'string') continue
    const stem = basename(filename).replace(/\.[^.]+$/, '')
    const candidates = [join(dirname(filename), `${stem}.d`)]
    if (stem.startsWith('lib')) candidates.push(join(dirname(filename), `${stem.slice(3)}.d`))
    for (const depfile of candidates) {
      if (!existsSync(depfile)) continue
      artifactDepfiles += 1
      for (const word of readFileSync(depfile, 'utf8').split(/\s+/)) {
        if (!word.endsWith('.rs')) continue
        const absolute = isAbsolute(word) ? resolve(word) : resolve(root, word)
        const path = relative(root, absolute)
        if (path.startsWith('..') || isAbsolute(path) || !existsSync(absolute)) continue
        covered.add(path.replaceAll('\\', '/'))
      }
      break
    }
  }
}
if (artifactDepfiles === 0 || covered.size === 0) {
  throw new Error('current cargo artifacts supplied no source depfiles; census coverage is unproven')
}
appendFileSync(rawPath, JSON.stringify({
  reason: 'harn-source-coverage',
  floor_bytes: Number(floorText),
  paths: [...covered].sort(),
}) + '\n')
