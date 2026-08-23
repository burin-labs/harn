// Machine-readable docs projections for agents.
//
// The HTML site is for people. This module writes the companion representation
// agents actually consume: `/llms.txt`, `/llms-full.txt`, and one `.md` file
// per page. Each page is the include-resolved Markdown plus a short agent
// header (summary, canonical Website URL, pre-1.0 version contract) and a
// "Read next" footer of `.md` URLs. The body is not dumped from HTML.
//
// Lives as .mjs so `node prerender.mjs` can import it without a TS loader.
export const SITE_ORIGIN = "https://harnlang.com"
export const LLMS_TXT_PATH = "llms.txt"
export const LLMS_FULL_TXT_PATH = "llms-full.txt"

export const AGENT_VERSION_NOTICE =
  "This page documents Harn, which is pre-1.0. Language, standard library, and CLI APIs may change. If the intended version is unclear, clarify before using this page."

const INDEX_SUMMARY =
  "Harn is a pipeline-oriented language and runtime for orchestrating AI agents."

const INDEX_STATUS =
  "Harn is pre-1.0. Prefer this file and the per-page `.md` URLs over scraping HTML. If the intended version is unclear, clarify before using these pages."

export function markdownUrlForSlug(slug) {
  return `/${slug}.md`
}

export function absoluteMarkdownUrl(slug) {
  return new URL(markdownUrlForSlug(slug), SITE_ORIGIN).href
}

export function absoluteHtmlUrl(slug) {
  return new URL(`/${slug}.html`, SITE_ORIGIN).href
}

export function agentPageFromDoc(page) {
  return {
    slug: page.slug,
    title: page.title,
    description: page.description,
    sectionTitle: page.sectionTitle,
    markdownSource: page.markdownSource,
    prev: page.prev ? { title: page.prev.title, slug: slugFromDocUrl(page.prev.url) } : null,
    next: page.next ? { title: page.next.title, slug: slugFromDocUrl(page.next.url) } : null,
  }
}

export function clientPagePayload(page) {
  const payload = { ...page }
  delete payload.markdownSource
  return payload
}

export function renderAgentPage(page) {
  const titleLine = `# ${page.title}`
  let body = page.markdownSource.trim()
  if (body === titleLine || body.startsWith(`${titleLine}\n`)) {
    body = body.slice(titleLine.length).trim()
  }

  const lines = [
    titleLine,
    "",
    `> ${page.description}`,
    "",
    `Website: ${absoluteHtmlUrl(page.slug)}`,
    "",
    AGENT_VERSION_NOTICE,
    "",
    "---",
    "",
    body,
    "",
  ]

  const nextLinks = [
    page.prev ? `- [${page.prev.title}](${absoluteMarkdownUrl(page.prev.slug)})` : null,
    page.next ? `- [${page.next.title}](${absoluteMarkdownUrl(page.next.slug)})` : null,
  ].filter((line) => line !== null)

  if (nextLinks.length > 0) {
    lines.push("---", "", "## Read next", "", ...nextLinks, "")
  }

  return lines.join("\n")
}

export function renderLlmsTxt(pages) {
  const sections = new Map()
  for (const page of pages) {
    const group = sections.get(page.sectionTitle) ?? []
    group.push(page)
    sections.set(page.sectionTitle, group)
  }

  const lines = [
    "# Harn",
    "",
    `> ${INDEX_SUMMARY}`,
    "",
    INDEX_STATUS,
    "",
    `Website: ${SITE_ORIGIN}/`,
    "",
    "## Docs",
    "",
  ]

  for (const [section, sectionPages] of sections) {
    lines.push(`### ${section}`, "")
    for (const page of sectionPages) {
      lines.push(`- [${page.title}](${absoluteMarkdownUrl(page.slug)}): ${page.description}`)
    }
    lines.push("")
  }

  lines.push(
    "## Optional",
    "",
    `- [Full documentation](${new URL(`/${LLMS_FULL_TXT_PATH}`, SITE_ORIGIN).href}): every page concatenated`,
    `- [Language quick reference](${new URL("/docs/llm/harn-quickref.md", SITE_ORIGIN).href})`,
    `- [Triggers quick reference](${new URL("/docs/llm/harn-triggers-quickref.md", SITE_ORIGIN).href})`,
    "",
  )

  return lines.join("\n")
}

export function renderLlmsFullTxt(pages) {
  const header = [
    "# Harn",
    "",
    `> ${INDEX_SUMMARY}`,
    "",
    INDEX_STATUS,
    "",
    `Website: ${SITE_ORIGIN}/`,
    "",
  ].join("\n")

  return [header, ...pages.map((page) => renderAgentPage(page))].join("\n---\n\n")
}

function slugFromDocUrl(url) {
  return url.replace(/^\//, "").replace(/\.html$/, "")
}
