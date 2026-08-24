import { readFileSync } from "node:fs"
import { join, posix } from "node:path"
import { visit, SKIP } from "unist-util-visit"
import {
  DIAGNOSTIC_DETAIL_CLASS,
  DIAGNOSTIC_FIGURE_CLASS,
  DIAGNOSTIC_HELP_CLASS,
  DIAGNOSTIC_REPAIR_CLASS,
  DIAGNOSTIC_SPAN_CLASS,
} from "../src/lib/diagnostic-markup.ts"

interface DiagnosticRepair {
  id: string
  safety: string
  summary: string
}

export interface DiagnosticExample {
  sourcePath: string
  blockIndex: number
  command: "check" | "lint"
  source: string
  diagnostic: {
    source: string
    severity: "error" | "warning"
    code: string
    message: string
    span: { start: number; end: number }
    help?: string | null
    repairs: DiagnosticRepair[]
  }
}

interface DiagnosticExamplesFile {
  schemaVersion: 1
  examples: DiagnosticExample[]
}

function diagnosticExampleKey(sourcePath: string, blockIndex: number): string {
  return `${sourcePath}#${blockIndex}`
}

export function loadDiagnosticExamples(repoRoot: string): Map<string, DiagnosticExample> {
  const file = join(repoRoot, "docs/diagnostic-examples.json")
  const parsed = JSON.parse(readFileSync(file, "utf8")) as DiagnosticExamplesFile
  if (parsed.schemaVersion !== 1 || !Array.isArray(parsed.examples)) {
    throw new Error("docs/diagnostic-examples.json has an unsupported schema")
  }
  const examples = new Map<string, DiagnosticExample>()
  for (const example of parsed.examples) {
    const key = diagnosticExampleKey(example.sourcePath, example.blockIndex)
    const diagnostic = example.diagnostic
    if (
      !example.sourcePath.startsWith("docs/src/") ||
      !Number.isInteger(example.blockIndex) ||
      example.blockIndex < 1 ||
      (example.command !== "check" && example.command !== "lint") ||
      typeof example.source !== "string" ||
      typeof diagnostic?.source !== "string" ||
      !diagnostic.code?.startsWith("HARN-") ||
      typeof diagnostic.message !== "string" ||
      diagnostic.message.length === 0 ||
      (diagnostic.severity !== "error" && diagnostic.severity !== "warning") ||
      !Number.isInteger(diagnostic.span?.start) ||
      !Number.isInteger(diagnostic.span?.end) ||
      diagnostic.span.start < 0 ||
      diagnostic.span.end <= diagnostic.span.start ||
      !Array.isArray(diagnostic.repairs) ||
      diagnostic.repairs.length === 0 ||
      diagnostic.repairs.some(
        (repair) =>
          typeof repair?.id !== "string" ||
          repair.id.length === 0 ||
          typeof repair.safety !== "string" ||
          repair.safety.length === 0 ||
          typeof repair.summary !== "string" ||
          repair.summary.length === 0,
      )
    ) {
      throw new Error(`invalid checked diagnostic projection: ${key}`)
    }
    if (examples.has(key)) throw new Error(`duplicate checked diagnostic projection: ${key}`)
    examples.set(key, example)
  }
  return examples
}

const OWNED_HARN_FENCES = new Set([
  "harn",
  "harn,check",
  "harn,diagnostic-check",
  "harn,diagnostic-lint",
  "harn,ignore",
  "harn-prompt",
  "harn-prompt,ignore",
])

function diagnosticFenceCommand(language: string): "check" | "lint" | null {
  if (language === "harn,diagnostic-check") return "check"
  if (language === "harn,diagnostic-lint") return "lint"
  return null
}

// Match the Harn checker's per-file fence index, then attach only an opaque
// projection key to the Markdown node. Diagnostic prose and codes stay in the
// generated sidecar; the Markdown never becomes a second source of truth.
export function remarkCheckedDiagnostics(
  sourceRel: string,
  examples: Map<string, DiagnosticExample>,
  seen: Set<string>,
) {
  const sourcePath = posix.join("docs/src", sourceRel)
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (tree: any) => {
    let harnIndex = 0
    visit(tree, "code", (node: any) => {
      const language = typeof node.lang === "string" ? node.lang : ""
      if (!OWNED_HARN_FENCES.has(language)) return
      harnIndex += 1
      const command = diagnosticFenceCommand(language)
      if (command == null) return
      const key = diagnosticExampleKey(sourcePath, harnIndex)
      const example = examples.get(key)
      if (!example) throw new Error(`missing checked diagnostic projection: ${key}`)
      const source = `${node.value}\n`
      if (example.command !== command || example.source !== source) {
        throw new Error(`stale checked diagnostic projection: ${key}`)
      }
      node.data ??= {}
      node.data.hProperties = {
        ...(node.data.hProperties ?? {}),
        dataDiagnosticKey: key,
      }
      seen.add(key)
    })
  }
}

function stringIndexForByteOffset(source: string, target: number): number {
  const encoder = new TextEncoder()
  let bytes = 0
  let index = 0
  for (const character of source) {
    if (bytes === target) return index
    bytes += encoder.encode(character).length
    index += character.length
    if (bytes > target) break
  }
  if (bytes === target) return index
  throw new Error(`diagnostic byte offset ${target} is not a UTF-8 boundary`)
}

// Highlight.js has already split source into nested token spans. Walk their
// text nodes in source order and wrap every overlap so the compiler's one byte
// span remains visible without discarding syntax highlighting.
function markDiagnosticRange(
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  node: any,
  start: number,
  end: number,
  title: string,
  severity: "error" | "warning",
  cursor: { value: number },
) {
  if (!Array.isArray(node.children)) return
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const marked: any[] = []
  for (const child of node.children) {
    if (child.type !== "text") {
      markDiagnosticRange(child, start, end, title, severity, cursor)
      marked.push(child)
      continue
    }
    const nodeStart = cursor.value
    const nodeEnd = nodeStart + child.value.length
    cursor.value = nodeEnd
    const overlapStart = Math.max(start, nodeStart)
    const overlapEnd = Math.min(end, nodeEnd)
    if (overlapStart >= overlapEnd) {
      marked.push(child)
      continue
    }
    const localStart = overlapStart - nodeStart
    const localEnd = overlapEnd - nodeStart
    if (localStart > 0) marked.push({ type: "text", value: child.value.slice(0, localStart) })
    marked.push({
      type: "element",
      tagName: "span",
      properties: {
        className: [DIAGNOSTIC_SPAN_CLASS, `${DIAGNOSTIC_SPAN_CLASS}-${severity}`],
        title,
      },
      children: [{ type: "text", value: child.value.slice(localStart, localEnd) }],
    })
    if (localEnd < child.value.length) {
      marked.push({ type: "text", value: child.value.slice(localEnd) })
    }
  }
  node.children = marked
}

function diagnosticParagraph(
  className: string,
  label: string,
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  children: any[],
) {
  return {
    type: "element",
    tagName: "p",
    properties: { className: [className] },
    children: [
      {
        type: "element",
        tagName: "strong",
        properties: {},
        children: [{ type: "text", value: label }],
      },
      { type: "text", value: " " },
      ...children,
    ],
  }
}

export function rehypeCheckedDiagnostics(examples: Map<string, DiagnosticExample>) {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (tree: any) => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    visit(tree, "element", (node: any, index: number | undefined, parent: any) => {
      if (node.tagName !== "pre" || parent == null || index == null) return
      const code = node.children?.find(
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        (child: any) => child.type === "element" && child.tagName === "code",
      )
      const key = code?.properties?.dataDiagnosticKey
      if (typeof key !== "string") return
      delete code.properties.dataDiagnosticKey
      const example = examples.get(key)
      if (!example) throw new Error(`unknown checked diagnostic projection: ${key}`)
      const diagnostic = example.diagnostic
      const start = stringIndexForByteOffset(example.source, diagnostic.span.start)
      const end = stringIndexForByteOffset(example.source, diagnostic.span.end)
      const title = `${diagnostic.severity}[${diagnostic.code}]: ${diagnostic.message}`
      markDiagnosticRange(code, start, end, title, diagnostic.severity, { value: 0 })

      const detailId = `diagnostic-${key.replace(/[^a-z0-9]+/gi, "-").toLowerCase()}`
      node.properties = { ...node.properties, ariaDescribedBy: detailId }
      const detailChildren = [
        diagnosticParagraph(DIAGNOSTIC_DETAIL_CLASS + "-summary", title, []),
      ]
      if (diagnostic.help) {
        detailChildren.push(
          diagnosticParagraph(DIAGNOSTIC_HELP_CLASS, "Help:", [
            { type: "text", value: diagnostic.help },
          ]),
        )
      }
      for (const repair of diagnostic.repairs) {
        detailChildren.push(
          diagnosticParagraph(DIAGNOSTIC_REPAIR_CLASS, "Repair:", [
            { type: "text", value: `${repair.summary} (` },
            {
              type: "element",
              tagName: "code",
              properties: {},
              children: [{ type: "text", value: repair.id }],
            },
            { type: "text", value: `, ${repair.safety})` },
          ]),
        )
      }
      parent.children[index] = {
        type: "element",
        tagName: "figure",
        properties: {
          className: [DIAGNOSTIC_FIGURE_CLASS, `${DIAGNOSTIC_FIGURE_CLASS}-${diagnostic.severity}`],
        },
        children: [
          node,
          {
            type: "element",
            tagName: "figcaption",
            properties: { className: [DIAGNOSTIC_DETAIL_CLASS], id: detailId },
            children: detailChildren,
          },
        ],
      }
      return SKIP
    })
  }
}
