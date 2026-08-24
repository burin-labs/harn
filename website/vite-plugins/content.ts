// Build-time documentation pipeline.
//
// Reads the Diataxis-structured Markdown under docs/src (the same content
// mdBook consumed, kept in place so every generator/checker — check-docs-snippets,
// the language-spec mirror, diagnostics, harn-keywords.js — is unaffected) and
// produces the navigation tree, per-page rendered HTML, and a search index that
// the React site and the SSG prerender consume.
//
// Markdown → HTML happens here, in Node, so the client never ships a markdown
// parser or a syntax highlighter. Harn code blocks are highlighted with the
// grammar ported from docs/theme/harn-hljs.js, fed by the Rust-generated keyword
// table docs/theme/harn-keywords.js.
//
// The one thing this stage cannot finish is a Mermaid diagram: laying one out
// needs real text metrics, so ```mermaid fences become a figure here and the
// browser renders the SVG into it (src/lib/mermaid.ts) on the handful of pages
// that have one.
import { readFileSync, readdirSync, statSync } from "node:fs"
import { join, dirname, relative, posix } from "node:path"
import matter from "gray-matter"
import { unified } from "unified"
import remarkParse from "remark-parse"
import remarkGfm from "remark-gfm"
import remarkRehype from "remark-rehype"
import rehypeRaw from "rehype-raw"
import rehypeSlug from "rehype-slug"
import rehypeHighlight from "rehype-highlight"
import rehypeStringify from "rehype-stringify"
import { all as allLanguages } from "lowlight"
import { visit, SKIP } from "unist-util-visit"
import { toText } from "hast-util-to-text"
import {
  DIAGRAM_CANVAS_CLASS,
  DIAGRAM_FIGURE_CLASS,
  DIAGRAM_SOURCE_CLASS,
  DIAGRAM_SOURCE_LABEL,
} from "../src/lib/diagram-markup.ts"
import { CODE_FIGURE_CLASS, CODE_FILENAME_CLASS } from "../src/lib/code-markup.ts"
import { SYSTEMS, CAPABILITIES, RATINGS, type Rating } from "../src/data/comparison.ts"
import {
  loadDiagnosticExamples,
  remarkCheckedDiagnostics,
  rehypeCheckedDiagnostics,
  type DiagnosticExample,
} from "./diagnostics.ts"

export interface Heading {
  depth: number
  id: string
  text: string
}

export interface DocMeta {
  slug: string
  url: string
  title: string
  description: string
  navTitle: string
  sectionId: string
  sectionTitle: string
  sourceRel: string
  editUrl: string
}

export interface PageData extends DocMeta {
  html: string
  headings: Heading[]
  prev: { title: string; url: string } | null
  next: { title: string; url: string } | null
  // Include-resolved Markdown used only to emit the agent `.md` projection.
  // Stripped from the client `_content/*.json` payload in prerender.
  markdownSource: string
}

export interface NavItem {
  title: string
  url: string
  slug: string
  children: NavItem[]
}

export interface NavGroup {
  heading: string | null
  items: NavItem[]
}

export interface NavSection {
  id: string
  title: string
  url: string
  groups: NavGroup[]
}

export interface SearchDoc {
  slug: string
  url: string
  title: string
  sectionTitle: string
  headings: string[]
  text: string
}

export interface LoadedDocs {
  nav: NavSection[]
  meta: Record<string, DocMeta>
  pages: Map<string, PageData>
  search: SearchDoc[]
  // SUMMARY.md page order, excluding missing files. Used by the agent index.
  order: string[]
}

const GITHUB_EDIT_BASE = "https://github.com/burin-labs/harn/edit/main/docs/src/"
const GITHUB_BLOB_BASE = "https://github.com/burin-labs/harn/blob/main/"
const DESCRIPTION_LIMIT = 180

function decodeEntity(value: string): string {
  const named: Record<string, string> = {
    amp: "&",
    gt: ">",
    lt: "<",
    quot: '"',
    apos: "'",
    nbsp: " ",
  }
  if (value in named) return named[value]
  const decimal = value.match(/^#(\d+)$/)
  const hex = value.match(/^#x([0-9a-f]+)$/i)
  const code = decimal ? Number(decimal[1]) : hex ? Number.parseInt(hex[1], 16) : NaN
  if (!Number.isFinite(code) || code < 0 || code > 0x10ffff) return ""
  return String.fromCodePoint(code)
}

function textFromHtml(html: string): string {
  return html
    .replace(/<script\b[\s\S]*?<\/script>/gi, " ")
    .replace(/<style\b[\s\S]*?<\/style>/gi, " ")
    .replace(/<[^>]+>/g, " ")
    .replace(/&([a-z]+|#\d+|#x[0-9a-f]+);/gi, (_, entity: string) => decodeEntity(entity))
    .replace(/\s+/g, " ")
    .trim()
}

// Diagram sources are markup, not prose: their node ids and arrow syntax would
// otherwise land in the search index as noise.
function stripDiagramSources(html: string): string {
  const figure = new RegExp(`<figure class="${DIAGRAM_FIGURE_CLASS}">[\\s\\S]*?</figure>`, "gi")
  return html.replace(figure, " ")
}

function truncateDescription(text: string): string {
  if (text.length <= DESCRIPTION_LIMIT) return text
  const clipped = text.slice(0, DESCRIPTION_LIMIT - 3)
  const lastSpace = clipped.lastIndexOf(" ")
  return `${clipped.slice(0, lastSpace > 80 ? lastSpace : clipped.length).trimEnd()}...`
}

function descriptionFromHtml(html: string, fallback: string): string {
  const firstParagraph = html.match(/<p(?:\s[^>]*)?>([\s\S]*?)<\/p>/i)?.[1]
  return truncateDescription(textFromHtml(firstParagraph ?? html) || fallback)
}

// Maps SUMMARY.md parts (the `# Heading` groups) to the top-level section tabs.
//
// "Internals" rather than the Diataxis label "Explanation": that part holds
// architecture, protocol contributions, and ADRs, so it explains how Harn is
// built rather than teaching a reader the concepts they need to use it. The
// concept material has its own part, folded into Introduction.
//
// Migrations has no tab. Version-upgrade instructions are how-to material, and
// they sit under How-to guides rather than claiming a seventh tab of their own.
const SECTION_TABS: { id: string; title: string; parts: string[] }[] = [
  { id: "introduction", title: "Introduction", parts: ["Introduction", "Concepts"] },
  { id: "tutorials", title: "Tutorials", parts: ["Tutorials"] },
  { id: "guides", title: "Guides", parts: ["How-to guides"] },
  { id: "reference", title: "Reference", parts: ["Reference"] },
  { id: "explanation", title: "Internals", parts: ["Internals"] },
  { id: "operations", title: "Operating Harn", parts: ["Operating Harn"] },
]

// ---------------------------------------------------------------------------
// Harn highlight grammar (ported from docs/theme/harn-hljs.js)
// ---------------------------------------------------------------------------

function loadHarnKeywords(repoRoot: string): {
  keyword: string
  literal: string
  built_in: string
} {
  const file = join(repoRoot, "spec/language-vocabulary.json")
  const vocabulary = JSON.parse(readFileSync(file, "utf8")) as {
    keywords: string[]
    literals: string[]
    builtins: string[]
  }
  return {
    keyword: vocabulary.keywords.join(" "),
    literal: vocabulary.literals.join(" "),
    built_in: vocabulary.builtins.join(" "),
  }
}

function makeHarnLanguage(keywords: {
  keyword: string
  literal: string
  built_in: string
}) {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return function harn(hljs: any) {
    const KEYWORDS = {
      keyword: keywords.keyword,
      literal: keywords.literal,
      built_in: keywords.built_in,
    }
    const INTERPOLATION: Record<string, unknown> = {
      className: "subst",
      begin: /\$\{/,
      end: /\}/,
      keywords: KEYWORDS,
      contains: [],
    }
    const STRING = {
      className: "string",
      begin: '"',
      end: '"',
      illegal: "\\n",
      contains: [{ begin: "\\\\." }, INTERPOLATION],
    }
    const DURATION = {
      className: "number",
      begin: /\b\d+(?:\.\d+)?(?:ms|s|m|h)\b/,
      relevance: 0,
    }
    const NUMBER = {
      className: "number",
      variants: [
        { begin: /\b\d+\.\d+(?:[eE][+-]?\d+)?/ },
        { begin: /\b\d+(?:[eE][+-]?\d+)?/ },
      ],
      relevance: 0,
    }
    const TYPE = {
      className: "type",
      begin: /\b[A-Z][A-Za-z0-9_]*\b/,
      relevance: 0,
    }
    const mainContains = [
      hljs.C_LINE_COMMENT_MODE,
      hljs.C_BLOCK_COMMENT_MODE,
      STRING,
      DURATION,
      NUMBER,
      TYPE,
      {
        className: "title.function",
        beginKeywords: "fn pipeline",
        end: /[(\s]/,
        excludeEnd: true,
        contains: [{ begin: /[a-z_][A-Za-z0-9_]*/ }],
        relevance: 0,
      },
    ]
    INTERPOLATION.contains = mainContains
    return {
      name: "Harn",
      aliases: ["harn"],
      keywords: KEYWORDS,
      contains: mainContains,
    }
  }
}

// ---------------------------------------------------------------------------
// Prompt-template highlight grammar
// ---------------------------------------------------------------------------

// `harn-prompt` fences hold prompt-template source, whose vocabulary lives in
// crates/harn-vm/src/stdlib/template/vocabulary.rs and is projected by
// `make gen-prompt-grammar` into the VS Code TextMate grammar. Read the
// keyword/filter matchers straight out of that generated grammar instead of
// restating them here, so the editor and the docs site cannot drift apart.
interface TmMatcher {
  match?: string
}

function tmAlternation(matcher: TmMatcher | undefined): string[] {
  const source = matcher?.match ?? ""
  const group = /\(([A-Za-z_|]+)\)/.exec(source)
  if (!group) return []
  return group[1].split("|").filter(Boolean)
}

function makeHarnPromptLanguage(repoRoot: string) {
  const file = join(repoRoot, "editors/vscode/syntaxes/harn-prompt.tmLanguage.json")
  const grammar = JSON.parse(readFileSync(file, "utf8")) as {
    repository: Record<string, TmMatcher | undefined>
  }
  const keywords = tmAlternation(grammar.repository["keyword"])
  const literals = tmAlternation(grammar.repository["literal-keyword"])
  const filters = tmAlternation(grammar.repository["filter"])
  if (keywords.length === 0 || literals.length === 0 || filters.length === 0) {
    throw new Error(
      "harn-prompt.tmLanguage.json no longer exposes keyword/literal/filter alternations; " +
        "update makeHarnPromptLanguage to match the regenerated grammar",
    )
  }

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return function harnPrompt(_hljs: any) {
    const DIRECTIVE = {
      className: "template-variable",
      begin: /\{\{-?/,
      end: /-?\}\}/,
      keywords: {
        keyword: keywords.join(" "),
        literal: literals.join(" "),
        built_in: filters.join(" "),
      },
      contains: [
        { className: "string", begin: '"', end: '"', contains: [{ begin: /\\./ }] },
        { className: "string", begin: "'", end: "'", contains: [{ begin: /\\./ }] },
        { className: "number", begin: /\b\d+(?:\.\d+)?/, relevance: 0 },
      ],
    }
    return {
      name: "Harn prompt template",
      aliases: ["harn-prompt"],
      contains: [
        { className: "comment", begin: /\{\{#/, end: /#\}\}/ },
        DIRECTIVE,
      ],
    }
  }
}

// ---------------------------------------------------------------------------
// mdBook {{#include}} resolution
// ---------------------------------------------------------------------------

const INCLUDE_RE = /\{\{#include\s+([^}]+)\}\}/g

function resolveIncludes(raw: string, fileAbs: string, repoRoot: string): string {
  return raw.replace(INCLUDE_RE, (_match, spec: string) => {
    const trimmed = spec.trim()
    // path[:start[:end]] — 1-indexed, inclusive (mdBook anchors by line range).
    const parts = trimmed.split(":")
    const relPath = parts[0]
    const target = join(dirname(fileAbs), relPath)
    let content: string
    try {
      content = readFileSync(target, "utf8")
    } catch {
      void repoRoot
      return `> _(missing include: ${relPath})_`
    }
    if (parts.length === 1) return content
    const lines = content.split("\n")
    const start = parts[1] ? parseInt(parts[1], 10) : 1
    const end = parts[2] ? parseInt(parts[2], 10) : lines.length
    return lines.slice(Math.max(0, start - 1), end).join("\n")
  })
}

// ---------------------------------------------------------------------------
// Comparison matrix
// ---------------------------------------------------------------------------

const COMPARISON_MATRIX_RE = /\{\{#comparison-matrix\}\}/g
const COMPARISON_MARKER = "comparison-matrix"
const ISSUE_BASE = "https://github.com/burin-labs/harn/issues"

const RATING_LABEL: Record<Rating, string> = {
  yes: "Yes",
  partial: "Partial",
  no: "No",
  unknown: "—",
}

/**
 * Expand `{{#comparison-matrix}}` into a real Markdown table.
 *
 * The table is emitted with every system, in `SYSTEMS` order, so the
 * prerendered page, a reader with JavaScript off, and the Markdown projection
 * an agent reads all get the complete comparison. Narrowing to the default
 * columns is progressive enhancement layered on top by `useComparisonMatrix`.
 *
 * An HTML comment marks the table for the rehype pass below. Markdown cells
 * cannot carry attributes, so per-cell identity and the hover notes are
 * attached there instead of being inlined as raw HTML here — that keeps
 * `markdownSource` a genuine Markdown table.
 */
function resolveComparisonMatrix(raw: string): string {
  if (!COMPARISON_MATRIX_RE.test(raw)) return raw
  COMPARISON_MATRIX_RE.lastIndex = 0

  const header = ["Capability", ...SYSTEMS.map((s) => s.name)]
  const rows = CAPABILITIES.map((cap) => {
    const cells = SYSTEMS.map((sys) => {
      const cell = RATINGS[cap.id]?.[sys.id]
      return RATING_LABEL[cell?.rating ?? "unknown"]
    })
    return [`[${cap.label}](#${cap.id})`, ...cells]
  })

  const table = [
    `| ${header.join(" | ")} |`,
    `|${header.map(() => "---").join("|")}|`,
    ...rows.map((r) => `| ${r.join(" | ")} |`),
  ].join("\n")

  // Rows with tracked work, emitted from the same data as the table. A plan
  // cannot appear here without an issue number, because the type requires one,
  // so a reader can always check whether the intent is still live.
  const planned = CAPABILITIES.filter((cap) => cap.plan)
  const plans = planned.length
    ? [
        "",
        "**Work in progress.** These rows have tracked work behind them. Each",
        "links the issue, so you can see for yourself whether it is still moving.",
        "",
        ...planned.map(
          (cap) =>
            `- [${cap.label}](#${cap.id}) — ${cap.plan!.note} ` +
            `([#${cap.plan!.issue}](${ISSUE_BASE}/${cap.plan!.issue}))`,
        ),
      ].join("\n")
    : ""

  // Systems whose vocabulary is mapped onto Harn's. These are the closest thing
  // to a migration path, so the reader who has just decided the table is in
  // their favour has somewhere to go next. Anchors are validated downstream:
  // a renamed heading on that page fails the build rather than rotting here.
  const mapped = SYSTEMS.filter((s) => s.comingFrom)
  const coming = mapped.length
    ? [
        "",
        "**Already using one of these?** These systems have a term-by-term map",
        "onto Harn's vocabulary, which is the shortest way to read your own",
        "workflow in Harn's terms.",
        "",
        ...mapped.map(
          (s) => `- [Coming from ${s.name}](./concepts/sota-comparison.md#${s.comingFrom})`,
        ),
      ].join("\n")
    : ""

  return raw.replace(
    COMPARISON_MATRIX_RE,
    `<!--${COMPARISON_MARKER}-->\n\n${table}\n${plans}\n${coming}`,
  )
}

/**
 * Attach per-cell identity to the comparison table and inject the column
 * picker ahead of it.
 *
 * Column and row identity come from `SYSTEMS` and `CAPABILITIES` order, which
 * is the same order the emitter above wrote, so the two stay consistent by
 * construction rather than by matching header text.
 */
function rehypeComparisonMatrix() {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (tree: any) => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    visit(tree, "comment", (node: any, index: number | undefined, parent: any) => {
      if (node.value?.trim() !== COMPARISON_MARKER || !parent || index === undefined) return
      const table = parent.children
        .slice(index + 1)
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        .find((n: any) => n.type === "element" && n.tagName === "table")
      if (!table) return

      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const rowsOf = (section: string): any[] => {
        const el = table.children.find(
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          (n: any) => n.type === "element" && n.tagName === section,
        )
        return el
          ? // eslint-disable-next-line @typescript-eslint/no-explicit-any
            el.children.filter((n: any) => n.type === "element" && n.tagName === "tr")
          : []
      }
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const cellsOf = (tr: any): any[] =>
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        tr.children.filter((n: any) => n.type === "element" && /^t[hd]$/.test(n.tagName))

      // Header: first cell is the capability label, the rest are systems.
      for (const tr of rowsOf("thead")) {
        cellsOf(tr).forEach((cell, i) => {
          if (i === 0) return
          const sys = SYSTEMS[i - 1]
          if (!sys) return
          cell.properties = { ...cell.properties, dataSystem: sys.id, scope: "col" }
        })
      }

      // Body: one row per capability, in declaration order.
      rowsOf("tbody").forEach((tr, rowIndex) => {
        const cap = CAPABILITIES[rowIndex]
        if (!cap) return
        cellsOf(tr).forEach((cell, i) => {
          if (i === 0) {
            cell.properties = {
              ...cell.properties,
              scope: "row",
              ...(cap.favorsOthers ? { dataFavorsOthers: "true" } : {}),
            }
            return
          }
          const sys = SYSTEMS[i - 1]
          if (!sys) return
          const rated = RATINGS[cap.id]?.[sys.id]
          cell.properties = {
            ...cell.properties,
            dataSystem: sys.id,
            dataRating: rated?.rating ?? "unknown",
            ...(rated?.note ? { title: rated.note } : {}),
          }
        })
      })

      parent.children[index] = comparisonLegend()
      return SKIP
    })
  }
}

/** The column picker. Inert without JavaScript, which is why it starts hidden. */
function comparisonLegend() {
  const optional = SYSTEMS.filter((s) => s.id !== "harn")
  return {
    type: "element",
    tagName: "div",
    properties: { className: ["cmp-legend"], dataComparisonLegend: "true", hidden: true },
    children: [
      {
        type: "element",
        tagName: "p",
        properties: { className: ["cmp-legend__label"] },
        children: [{ type: "text", value: "Compare Harn with" }],
      },
      {
        type: "element",
        tagName: "div",
        properties: { className: ["cmp-legend__options"] },
        children: optional.map((sys) => ({
          type: "element",
          tagName: "label",
          properties: { className: ["cmp-legend__option"], title: sys.summary },
          children: [
            {
              type: "element",
              tagName: "input",
              properties: {
                type: "checkbox",
                value: sys.id,
                ...(sys.shownByDefault ? { checked: true } : {}),
              },
              children: [],
            },
            { type: "text", value: ` ${sys.name}` },
          ],
        })),
      },
    ],
  }
}

// ---------------------------------------------------------------------------
// Rehype helpers
// ---------------------------------------------------------------------------

// Normalize fenced-code language hints: ```harn,ignore / ```harn,no_run all
// highlight as Harn; take the token before the first comma or space.
function rehypeNormalizeCodeLang() {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (tree: any) => {
    visit(tree, "element", (node: any) => {
      if (node.tagName !== "code" || !node.properties) return
      const classes = node.properties.className
      if (!Array.isArray(classes)) return
      node.properties.className = classes.map((c: unknown) => {
        if (typeof c === "string" && c.startsWith("language-")) {
          const lang = c.slice("language-".length)
          const base = lang.split(/[,\s]/)[0]
          return "language-" + base
        }
        return c
      })
    })
  }
}

// A fence may name the file it came from: ```harn title="example.harn".
// remark keeps everything after the language word in `node.meta` and
// remark-rehype drops it, so lift it onto the element before that happens.
// Only `title` is recognised; an unknown key is left alone rather than guessed
// at, so a typo shows up as a missing header instead of a wrong one.
const FENCE_TITLE = /(?:^|\s)title="([^"]+)"/

function remarkCodeMeta() {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (tree: any) => {
    visit(tree, "code", (node: any) => {
      const title = typeof node.meta === "string" ? FENCE_TITLE.exec(node.meta)?.[1] : undefined
      if (!title) return
      node.data ??= {}
      // The hast property name, not the HTML attribute name: rehype-raw
      // re-parses the tree, and the reparse normalizes `data-file` to
      // `dataFile`. Writing the attribute spelling here works right up until
      // that reparse, then silently stops matching downstream.
      node.data.hProperties = { ...(node.data.hProperties ?? {}), dataFile: title }
    })
  }
}

// Wrap a titled code block in a figure with the filename as its caption. The
// caption is a real <figcaption>, so the association with the code survives for
// a screen reader rather than being a floating line of text above it.
function rehypeCodeTitle() {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (tree: any) => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    visit(tree, "element", (node: any, index: number | undefined, parent: any) => {
      if (node.tagName !== "pre" || parent == null || index == null) return
      // remark-rehype applies a code node's hProperties to the inner <code>,
      // not to the <pre> it wraps, so the filename arrives one level down.
      const code = node.children?.find(
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        (child: any) => child.type === "element" && child.tagName === "code",
      )
      const file = code?.properties?.dataFile
      if (typeof file !== "string" || file === "") return
      delete code.properties.dataFile
      parent.children[index] = {
        type: "element",
        tagName: "figure",
        properties: { className: [CODE_FIGURE_CLASS] },
        children: [
          {
            type: "element",
            tagName: "figcaption",
            properties: { className: [CODE_FILENAME_CLASS] },
            children: [{ type: "text", value: file }],
          },
          node,
        ],
      }
      return SKIP
    })
  }
}

// ```mermaid fences are diagrams, not code. Rewrite them into a figure the
// client turns into an SVG (see src/lib/mermaid.ts), keeping the source in a
// collapsed <details> so the diagram always has a text alternative — one that
// also stands in when JavaScript never runs. The source lives only in that
// <details>; the renderer reads it back from the DOM rather than from a
// duplicated data attribute.
function rehypeMermaid() {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (tree: any) => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    visit(tree, "element", (node: any, index: number | undefined, parent: any) => {
      if (node.tagName !== "pre" || parent == null || index == null) return
      const code = (node.children ?? []).find(
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        (c: any) => c.type === "element" && c.tagName === "code",
      )
      const classes = code?.properties?.className
      if (!Array.isArray(classes) || !classes.includes("language-mermaid")) return

      parent.children[index] = {
        type: "element",
        tagName: "figure",
        properties: { className: [DIAGRAM_FIGURE_CLASS] },
        children: [
          {
            type: "element",
            tagName: "div",
            // Focusable so a diagram wider than the column can be scrolled
            // from the keyboard. It is `display: none` until a render lands,
            // so it never becomes an empty tab stop.
            properties: { className: [DIAGRAM_CANVAS_CLASS], tabIndex: 0 },
            children: [],
          },
          {
            type: "element",
            tagName: "details",
            properties: { className: [DIAGRAM_SOURCE_CLASS] },
            children: [
              {
                type: "element",
                tagName: "summary",
                properties: {},
                children: [{ type: "text", value: DIAGRAM_SOURCE_LABEL }],
              },
              node,
            ],
          },
        ],
      }
      return SKIP
    })
  }
}

// A container that scrolls sideways has to be reachable without a mouse
// (WCAG 2.1.1), which means it needs to be focusable. Wide tables get a
// focusable wrapper — putting `display: block` on the <table> itself would win
// the scrolling and lose the row/column semantics screen readers announce — and
// code blocks become focusable in place.
function rehypeScrollableRegions() {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (tree: any) => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const wrapped = new WeakSet<any>()
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    visit(tree, "element", (node: any, index: number | undefined, parent: any) => {
      if (node.tagName === "pre") {
        node.properties = { ...node.properties, tabIndex: 0 }
        return SKIP
      }
      if (node.tagName !== "table" || parent == null || index == null) return
      if (wrapped.has(node)) return SKIP
      wrapped.add(node)
      const header = node.children
        ?.find((child: any) => child.tagName === "thead")
        ?.children?.find((child: any) => child.tagName === "tr")
      const columnCount = header?.children?.filter(
        (child: any) => child.tagName === "th" || child.tagName === "td",
      ).length ?? 0
      parent.children[index] = {
        type: "element",
        tagName: "div",
        properties: {
          className: columnCount >= 5 ? ["table-scroll", "table-scroll-wide"] : ["table-scroll"],
          tabIndex: 0,
        },
        children: [node],
      }
      return SKIP
    })
  }
}

// GitHub-style alert blockquotes → the shared `.callout` markup/classes used by
// the companion marketing site's <Callout> component (see
// website/docs-stack-conventions.md). `> [!NOTE]` / `[!TIP]` / `[!IMPORTANT]` /
// `[!WARNING]` / `[!CAUTION]` become styled callouts; every other blockquote is
// left untouched. Icon paths are kept in sync with that Callout component.
const ALERT_ICONS = {
  note: "M12 9v4m0 4h.01M12 3a9 9 0 100 18 9 9 0 000-18z",
  tip: "M9.663 17h4.673M12 3v1m0 12a4 4 0 01-2-7.465A4 4 0 1116 9a4 4 0 01-2 3.465V16a2 2 0 01-2 2 2 2 0 01-2-2v-.535",
  info: "M13 16h-1v-4h-1m1-4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z",
  warning:
    "M12 9v3.75m-9.303 3.376c-.866 1.5.217 3.374 1.948 3.374h14.71c1.73 0 2.813-1.874 1.948-3.374L13.949 3.378c-.866-1.5-3.032-1.5-3.898 0L2.697 16.126zM12 15.75h.007v.008H12v-.008z",
}

// Marker → (callout style bucket, human label, icon).
const ALERT_KINDS: Record<string, { cls: keyof typeof ALERT_ICONS; title: string }> = {
  NOTE: { cls: "note", title: "Note" },
  TIP: { cls: "tip", title: "Tip" },
  IMPORTANT: { cls: "info", title: "Important" },
  WARNING: { cls: "warning", title: "Warning" },
  CAUTION: { cls: "warning", title: "Caution" },
}

function alertIcon(cls: keyof typeof ALERT_ICONS) {
  return {
    type: "element",
    tagName: "svg",
    properties: {
      className: ["callout-icon"],
      fill: "none",
      viewBox: "0 0 24 24",
      stroke: "currentColor",
      strokeWidth: 1.8,
      ariaHidden: "true",
    },
    children: [
      {
        type: "element",
        tagName: "path",
        properties: { strokeLinecap: "round", strokeLinejoin: "round", d: ALERT_ICONS[cls] },
        children: [],
      },
    ],
  }
}

function rehypeGithubAlerts() {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (tree: any) => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    visit(tree, "element", (node: any, index: number | undefined, parent: any) => {
      if (node.tagName !== "blockquote" || parent == null || index == null) return
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const firstP = (node.children ?? []).find((c: any) => c.type === "element" && c.tagName === "p")
      if (!firstP) return
      const firstText = firstP.children?.[0]
      if (!firstText || firstText.type !== "text") return
      const match = /^\[!(NOTE|TIP|IMPORTANT|WARNING|CAUTION)\][ \t]*\r?\n?/.exec(firstText.value)
      if (!match) return
      const kind = ALERT_KINDS[match[1]]

      // Strip the `[!TYPE]` marker from the leading text, then drop the first
      // paragraph entirely if nothing but the marker was on it.
      firstText.value = firstText.value.slice(match[0].length)
      let body = node.children ?? []
      if (toText(firstP).trim() === "") {
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        body = body.filter((c: any) => c !== firstP)
      }

      parent.children[index] = {
        type: "element",
        tagName: "div",
        properties: { className: ["callout", "callout-" + kind.cls] },
        children: [
          alertIcon(kind.cls),
          {
            type: "element",
            tagName: "div",
            properties: { className: ["callout-body"] },
            children: [
              {
                type: "element",
                tagName: "p",
                properties: { className: ["callout-title"] },
                children: [{ type: "text", value: kind.title }],
              },
              ...body,
            ],
          },
        ],
      }
      return SKIP
    })
  }
}

function rehypeCollectHeadings(headings: Heading[]) {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (tree: any) => {
    visit(tree, "element", (node: any) => {
      const m = /^h([1-6])$/.exec(node.tagName ?? "")
      if (!m) return
      const depth = parseInt(m[1], 10)
      if (depth < 2 || depth > 3) return
      const id = node.properties?.id
      if (typeof id !== "string") return
      headings.push({ depth, id, text: toText(node).trim() })
    })
  }
}

function rehypeCaptureTitle(ref: { title: string | null }) {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (tree: any) => {
    visit(tree, "element", (node: any) => {
      if (ref.title === null && node.tagName === "h1") {
        ref.title = toText(node).trim()
      }
    })
  }
}

function rehypeCollectAnchors(anchors: Set<string>) {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (tree: any) => {
    visit(tree, "element", (node: any) => {
      const id = node.properties?.id
      if (typeof id === "string") anchors.add(id)
    })
  }
}

interface InternalLink {
  sourceSlug: string
  href: string
}

function rehypeCollectInternalLinks(sourceSlug: string, links: InternalLink[]) {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (tree: any) => {
    visit(tree, "element", (node: any) => {
      const href = node.properties?.href
      if (node.tagName === "a" && typeof href === "string" && href.length > 0) {
        if (href.startsWith("#") || href.startsWith("/")) links.push({ sourceSlug, href })
      }
    })
  }
}

// Rewrite intra-doc links: ./foo.md, ../bar/baz.md(#anchor) → /<slug>.html(#anchor),
// resolved relative to the source file. External links get target/rel.
function rehypeRewriteLinks(sourceRel: string) {
  const sourceRepoRel = posix.normalize(posix.join("docs/src", sourceRel))
  const dir = posix.dirname(sourceRepoRel)
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (tree: any) => {
    visit(tree, "element", (node: any) => {
      if (node.tagName !== "a" || !node.properties) return
      const href = node.properties.href
      if (typeof href !== "string" || href.length === 0) return
      if (/^(https?:)?\/\//.test(href) || href.startsWith("mailto:")) {
        node.properties.target = "_blank"
        node.properties.rel = "noopener noreferrer"
        return
      }
      if (href.startsWith("#")) return
      const [path, anchor] = href.split("#")
      if (!/\.(md|html)$/.test(path)) return
      const targetRepoRel = posix.normalize(posix.join(dir, path))
      if (!targetRepoRel.startsWith("docs/src/")) {
        node.properties.href = GITHUB_BLOB_BASE + targetRepoRel + (anchor ? "#" + anchor : "")
        node.properties.target = "_blank"
        node.properties.rel = "noopener noreferrer"
        return
      }
      const siteRel = targetRepoRel.slice("docs/src/".length).replace(/\.(md|html)$/, ".html")
      node.properties.href = "/" + siteRel + (anchor ? "#" + anchor : "")
    })
  }
}

// ---------------------------------------------------------------------------
// Markdown rendering
// ---------------------------------------------------------------------------

function buildProcessor(
  sourceRel: string,
  sourceSlug: string,
  headings: Heading[],
  anchors: Set<string>,
  links: InternalLink[],
  titleRef: { title: string | null },
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  languages: Record<string, any>,
  diagnosticExamples: Map<string, DiagnosticExample>,
  seenDiagnosticExamples: Set<string>,
) {
  return unified()
    .use(remarkParse)
    .use(remarkGfm)
    .use(remarkCodeMeta)
    .use(remarkCheckedDiagnostics, sourceRel, diagnosticExamples, seenDiagnosticExamples)
    .use(remarkRehype, { allowDangerousHtml: true })
    .use(rehypeRaw)
    .use(rehypeComparisonMatrix)
    .use(rehypeGithubAlerts)
    .use(rehypeNormalizeCodeLang)
    .use(rehypeMermaid)
    .use(rehypeHighlight, {
      detect: false,
      languages,
      // JSON Lines is JSON per line; highlight.js has no separate grammar.
      aliases: { json: ["jsonl"] },
    })
    .use(rehypeCheckedDiagnostics, diagnosticExamples)
    .use(rehypeCodeTitle)
    .use(rehypeScrollableRegions)
    .use(rehypeSlug)
    .use(rehypeCollectHeadings, headings)
    .use(rehypeCollectAnchors, anchors)
    .use(rehypeCaptureTitle, titleRef)
    .use(rehypeRewriteLinks, sourceRel)
    .use(rehypeCollectInternalLinks, sourceSlug, links)
    .use(rehypeStringify, { allowDangerousHtml: true })
}

// ---------------------------------------------------------------------------
// SUMMARY.md parsing
// ---------------------------------------------------------------------------

interface RawItem {
  title: string
  slug: string
  depth: number
  children: RawItem[]
}
interface RawGroup {
  heading: string | null
  items: RawItem[]
}
interface RawPart {
  title: string
  groups: RawGroup[]
}

const LINK_RE = /^(\s*)(?:- )?\[([^\]]+)\]\(([^)]+)\)/

function hrefToSlug(href: string): string {
  let h = href.replace(/^\.\//, "").replace(/^\//, "")
  h = h.replace(/\.md$/, "")
  return h
}

function parseSummary(repoRoot: string): { parts: RawPart[]; order: string[] } {
  const file = join(repoRoot, "docs/src/SUMMARY.md")
  const lines = readFileSync(file, "utf8").split("\n")
  const parts: RawPart[] = []
  const order: string[] = []

  let part: RawPart | null = null
  let group: RawGroup | null = null
  // Stack of items by depth for nesting.
  let stack: RawItem[] = []

  const ensurePart = (title: string) => {
    part = { title, groups: [] }
    parts.push(part)
    group = null
    stack = []
  }
  const ensureGroup = (heading: string | null) => {
    if (!part) ensurePart("Introduction")
    group = { heading, items: [] }
    part!.groups.push(group)
    stack = []
  }

  for (const line of lines) {
    if (line.startsWith("# ")) {
      ensurePart(line.slice(2).trim())
      continue
    }
    if (line.startsWith("## ")) {
      if (!part) ensurePart("Introduction")
      ensureGroup(line.slice(3).trim())
      continue
    }
    const m = LINK_RE.exec(line)
    if (!m) continue
    const indent = m[1].replace(/\t/g, "  ").length
    const depth = Math.floor(indent / 2)
    const title = m[2].trim()
    const slug = hrefToSlug(m[3].trim())
    if (!part) ensurePart("Introduction")
    if (!group) ensureGroup(null)
    const item: RawItem = { title, slug, depth, children: [] }
    if (depth === 0 || stack.length === 0) {
      group!.items.push(item)
      stack = [item]
    } else {
      while (stack.length > depth) stack.pop()
      const parent = stack[stack.length - 1]
      if (parent) parent.children.push(item)
      else group!.items.push(item)
      stack.push(item)
    }
    order.push(slug)
  }

  return { parts, order }
}

// ---------------------------------------------------------------------------
// Assembly
// ---------------------------------------------------------------------------

function collectMarkdownFiles(root: string): string[] {
  const out: string[] = []
  const walk = (dir: string) => {
    for (const entry of readdirSync(dir)) {
      const full = join(dir, entry)
      const st = statSync(full)
      if (st.isDirectory()) walk(full)
      else if (entry.endsWith(".md") && entry !== "SUMMARY.md") out.push(full)
    }
  }
  walk(root)
  return out
}

function decodedAnchor(anchor: string): string {
  try {
    return decodeURIComponent(anchor)
  } catch {
    return anchor
  }
}

function validateContentContract(
  pages: Map<string, PageData>,
  order: string[],
  anchorsBySlug: Map<string, Set<string>>,
  links: InternalLink[],
) {
  const errors: string[] = []
  const indexed = new Set(order)

  for (const slug of order) {
    if (!pages.has(slug)) errors.push(`SUMMARY.md points to missing page: ${slug}`)
  }
  for (const slug of pages.keys()) {
    if (!indexed.has(slug)) errors.push(`page is missing from SUMMARY.md: ${slug}`)
  }

  for (const { sourceSlug, href } of links) {
    let targetSlug = sourceSlug
    let anchor = ""
    if (href.startsWith("#")) {
      anchor = href.slice(1)
    } else {
      const [path, fragment = ""] = href.split("#", 2)
      if (!path.endsWith(".html")) continue
      targetSlug = path.slice(1, -".html".length)
      anchor = fragment
    }
    if (!pages.has(targetSlug)) {
      errors.push(`${sourceSlug}: ${href} points to missing page ${targetSlug}`)
      continue
    }
    if (anchor && !anchorsBySlug.get(targetSlug)?.has(decodedAnchor(anchor))) {
      errors.push(`${sourceSlug}: ${href} points to missing anchor`)
    }
  }

  if (errors.length > 0) {
    throw new Error(`documentation content contract failed:\n${errors.join("\n")}`)
  }
}

export function loadAllDocs(repoRoot: string): LoadedDocs {
  const srcRoot = join(repoRoot, "docs/src")
  const keywords = loadHarnKeywords(repoRoot)
  // Every highlight.js grammar lowlight ships, plus the two Harn grammars. This
  // is build-time only — no highlighter reaches the browser — so registering the
  // full set costs nothing at runtime and keeps `bash`, `json`, `toml`, `rust`,
  // `powershell`, `protobuf`, and friends highlighted wherever docs use them.
  const languages = {
    ...allLanguages,
    harn: makeHarnLanguage(keywords),
    "harn-prompt": makeHarnPromptLanguage(repoRoot),
  }
  const { parts, order } = parseSummary(repoRoot)
  const diagnosticExamples = loadDiagnosticExamples(repoRoot)
  const seenDiagnosticExamples = new Set<string>()

  // navTitle by slug (from SUMMARY link text), and section assignment.
  const navTitleBySlug = new Map<string, string>()
  const sectionBySlug = new Map<string, { id: string; title: string }>()

  const tabForPart = (partTitle: string) =>
    SECTION_TABS.find((t) => t.parts.includes(partTitle))

  const registerItem = (item: RawItem, tab: { id: string; title: string }) => {
    navTitleBySlug.set(item.slug, item.title)
    sectionBySlug.set(item.slug, tab)
    item.children.forEach((c) => registerItem(c, tab))
  }
  for (const part of parts) {
    const tab = tabForPart(part.title) ?? SECTION_TABS[0]
    for (const g of part.groups) g.items.forEach((i) => registerItem(i, tab))
  }

  // Render every markdown file.
  const pages = new Map<string, PageData>()
  const meta: Record<string, DocMeta> = {}
  const search: SearchDoc[] = []
  const anchorsBySlug = new Map<string, Set<string>>()
  const links: InternalLink[] = []
  const files = collectMarkdownFiles(srcRoot)

  for (const fileAbs of files) {
    const sourceRel = relative(srcRoot, fileAbs).split("\\").join("/")
    const slug = sourceRel.replace(/\.md$/, "")
    const raw = readFileSync(fileAbs, "utf8")
    const fm = matter(raw)
    const included = resolveComparisonMatrix(resolveIncludes(fm.content, fileAbs, repoRoot))

    const headings: Heading[] = []
    const anchors = new Set<string>()
    const titleRef: { title: string | null } = { title: null }
    const linkSourceRel =
      typeof fm.data.linkSourceRel === "string" ? fm.data.linkSourceRel : sourceRel
    const processor = buildProcessor(
      linkSourceRel,
      slug,
      headings,
      anchors,
      links,
      titleRef,
      languages,
      diagnosticExamples,
      seenDiagnosticExamples,
    )
    const html = String(processor.processSync(included))
    anchorsBySlug.set(slug, anchors)

    const fmTitle = typeof fm.data.title === "string" ? fm.data.title : null
    const navTitle = navTitleBySlug.get(slug) ?? fmTitle ?? titleRef.title ?? slug
    const title = fmTitle ?? titleRef.title ?? navTitle
    const description =
      typeof fm.data.description === "string"
        ? truncateDescription(fm.data.description)
        : descriptionFromHtml(html, title)
    const section = sectionBySlug.get(slug) ?? { id: "reference", title: "Reference" }
    const url = "/" + slug + ".html"

    const docMeta: DocMeta = {
      slug,
      url,
      title,
      description,
      navTitle,
      sectionId: section.id,
      sectionTitle: section.title,
      sourceRel,
      editUrl: GITHUB_EDIT_BASE + sourceRel,
    }
    meta[slug] = docMeta

    pages.set(slug, {
      ...docMeta,
      html,
      headings,
      prev: null,
      next: null,
      markdownSource: included,
    })

    const plain = textFromHtml(stripDiagramSources(html))
    search.push({
      slug,
      url,
      title,
      sectionTitle: section.title,
      headings: headings.map((h) => h.text),
      text: plain.slice(0, 1800),
    })
  }

  for (const key of diagnosticExamples.keys()) {
    if (!seenDiagnosticExamples.has(key)) {
      throw new Error(`checked diagnostic projection is not rendered by the site: ${key}`)
    }
  }

  validateContentContract(pages, order, anchorsBySlug, links)

  // prev/next from SUMMARY order (only slugs that resolved to real pages).
  const orderedExisting = order.filter((s) => pages.has(s))
  orderedExisting.forEach((slug, i) => {
    const page = pages.get(slug)!
    if (i > 0) {
      const p = pages.get(orderedExisting[i - 1])!
      page.prev = { title: p.navTitle, url: p.url }
    }
    if (i < orderedExisting.length - 1) {
      const n = pages.get(orderedExisting[i + 1])!
      page.next = { title: n.navTitle, url: n.url }
    }
  })

  // Build the nav tree (tabs → groups → items) from parts.
  const toNavItem = (item: RawItem): NavItem => ({
    title: item.title,
    slug: item.slug,
    url: "/" + item.slug + ".html",
    children: item.children.map(toNavItem),
  })

  const nav: NavSection[] = SECTION_TABS.map((tab) => {
    const tabParts = parts.filter((p) => tab.parts.includes(p.title))
    const multiPart = tabParts.length > 1
    const groups: NavGroup[] = []
    for (const part of tabParts) {
      for (const g of part.groups) {
        let heading = g.heading
        if (heading === null && multiPart) heading = part.title
        groups.push({ heading, items: g.items.map(toNavItem) })
      }
    }
    const firstSlug = groups[0]?.items[0]?.slug ?? ""
    return {
      id: tab.id,
      title: tab.title,
      url: firstSlug ? "/" + firstSlug + ".html" : "/",
      groups,
    }
  }).filter((s) => s.groups.length > 0)

  return { nav, meta, pages, search, order: orderedExisting }
}
