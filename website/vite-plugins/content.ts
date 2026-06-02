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
import { visit } from "unist-util-visit"
import { toText } from "hast-util-to-text"

export interface Heading {
  depth: number
  id: string
  text: string
}

export interface DocMeta {
  slug: string
  url: string
  title: string
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
}

const GITHUB_EDIT_BASE = "https://github.com/burin-labs/harn/edit/main/docs/src/"

// Maps SUMMARY.md parts (the `# Heading` groups) to the top-level section tabs.
// Mirrors docs/theme/harn-docs-nav.js, plus a Migrations tab so that part — which
// the old nav left orphaned — is reachable.
const SECTION_TABS: { id: string; title: string; parts: string[] }[] = [
  { id: "introduction", title: "Introduction", parts: ["Introduction", "Concepts"] },
  { id: "tutorials", title: "Tutorials", parts: ["Tutorials"] },
  { id: "guides", title: "Guides", parts: ["How-to guides"] },
  { id: "reference", title: "Reference", parts: ["Reference"] },
  { id: "explanation", title: "Explanation", parts: ["Explanation"] },
  { id: "operations", title: "Operations", parts: ["Operations"] },
  { id: "migrations", title: "Migrations", parts: ["Migrations"] },
]

// ---------------------------------------------------------------------------
// Harn highlight grammar (ported from docs/theme/harn-hljs.js)
// ---------------------------------------------------------------------------

function loadHarnKeywords(repoRoot: string): {
  keyword: string
  literal: string
  built_in: string
} {
  const file = join(repoRoot, "docs/theme/harn-keywords.js")
  const source = readFileSync(file, "utf8")
  const start = source.indexOf("{")
  const end = source.lastIndexOf("}")
  const literal = source.slice(start, end + 1)
  return new Function(`return (${literal});`)() as {
    keyword: string
    literal: string
    built_in: string
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

// Rewrite intra-doc links: ./foo.md, ../bar/baz.md(#anchor) → /<slug>.html(#anchor),
// resolved relative to the source file. External links get target/rel.
function rehypeRewriteLinks(sourceRel: string) {
  const dir = posix.dirname(sourceRel)
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
      let resolved = posix.normalize(posix.join(dir, path))
      resolved = resolved.replace(/\.(md|html)$/, ".html")
      node.properties.href = "/" + resolved + (anchor ? "#" + anchor : "")
    })
  }
}

// ---------------------------------------------------------------------------
// Markdown rendering
// ---------------------------------------------------------------------------

function buildProcessor(
  sourceRel: string,
  headings: Heading[],
  titleRef: { title: string | null },
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  harnLanguage: any,
) {
  return unified()
    .use(remarkParse)
    .use(remarkGfm)
    .use(remarkRehype, { allowDangerousHtml: true })
    .use(rehypeRaw)
    .use(rehypeNormalizeCodeLang)
    .use(rehypeHighlight, {
      detect: false,
      languages: { harn: harnLanguage },
    })
    .use(rehypeSlug)
    .use(rehypeCollectHeadings, headings)
    .use(rehypeCaptureTitle, titleRef)
    .use(rehypeRewriteLinks, sourceRel)
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

export function loadAllDocs(repoRoot: string): LoadedDocs {
  const srcRoot = join(repoRoot, "docs/src")
  const keywords = loadHarnKeywords(repoRoot)
  const harnLanguage = makeHarnLanguage(keywords)
  const { parts, order } = parseSummary(repoRoot)

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
  const files = collectMarkdownFiles(srcRoot)

  for (const fileAbs of files) {
    const sourceRel = relative(srcRoot, fileAbs).split("\\").join("/")
    const slug = sourceRel.replace(/\.md$/, "")
    const raw = readFileSync(fileAbs, "utf8")
    const fm = matter(raw)
    const included = resolveIncludes(fm.content, fileAbs, repoRoot)

    const headings: Heading[] = []
    const titleRef: { title: string | null } = { title: null }
    const processor = buildProcessor(sourceRel, headings, titleRef, harnLanguage)
    const html = String(processor.processSync(included))

    const fmTitle = typeof fm.data.title === "string" ? fm.data.title : null
    const navTitle = navTitleBySlug.get(slug) ?? fmTitle ?? titleRef.title ?? slug
    const title = fmTitle ?? titleRef.title ?? navTitle
    const section = sectionBySlug.get(slug) ?? { id: "reference", title: "Reference" }
    const url = "/" + slug + ".html"

    const docMeta: DocMeta = {
      slug,
      url,
      title,
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
    })

    const plain = html
      .replace(/<[^>]+>/g, " ")
      .replace(/\s+/g, " ")
      .trim()
    search.push({
      slug,
      url,
      title,
      sectionTitle: section.title,
      headings: headings.map((h) => h.text),
      text: plain.slice(0, 1800),
    })
  }

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

  return { nav, meta, pages, search }
}
