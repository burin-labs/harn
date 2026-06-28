import type { ReactNode } from "react"
import type { Mode } from "highlight.js"
import { createLowlight, type LanguageFn } from "lowlight"
import { KEYWORD_DOCS } from "./keyword-docs"

const harnLanguage: LanguageFn = (hljs) => {
  const keywords = {
    keyword: "agent_loop break continue each else fn for if in let parallel pipeline retry return spawn while",
    literal: "false nil true",
    built_in: "llm_call log read_file read_text tool_select",
  }
  const interpolation: Mode = {
    className: "subst",
    begin: /\$\{/,
    end: /\}/,
    keywords,
    contains: [],
  }
  const string: Mode = {
    className: "string",
    begin: '"',
    end: '"',
    illegal: "\\n",
    contains: [{ begin: "\\\\." }, interpolation],
  }
  const mainContains: Mode[] = [
    hljs.C_LINE_COMMENT_MODE,
    hljs.C_BLOCK_COMMENT_MODE,
    string,
    {
      className: "number",
      begin: /\b\d+(?:\.\d+)?(?:ms|s|m|h)?\b/,
      relevance: 0,
    },
    {
      className: "title.function",
      beginKeywords: "fn pipeline",
      end: /[(\s]/,
      excludeEnd: true,
      contains: [{ begin: /[a-z_][A-Za-z0-9_]*/ }],
      relevance: 0,
    },
  ]
  interpolation.contains = mainContains
  return {
    name: "Harn",
    aliases: ["harn"],
    keywords,
    contains: mainContains,
  }
}

type HighlightNode = {
  type: string
  value?: string
  tagName?: string
  properties?: { className?: string[] | string }
  children?: HighlightNode[]
}

const harnLowlight = createLowlight({ harn: harnLanguage })

// Token classes whose text we offer inline docs for (keywords + built-ins).
const TIPPABLE = new Set(["hljs-keyword", "hljs-built_in"])

function nodeText(node: HighlightNode): string {
  if (node.type === "text") return node.value ?? ""
  return (node.children ?? []).map(nodeText).join("")
}

function classList(node: HighlightNode): string[] {
  const className = node.properties?.className
  if (!className) return []
  return Array.isArray(className) ? className : [className]
}

function renderHighlightNode(node: HighlightNode, index: number): ReactNode {
  if (node.type === "text") return node.value ?? ""
  if (node.type !== "element") return null

  const classes = classList(node)
  // Offer a hover doc when this token is a keyword/built-in we have a note for.
  // The shared KeywordTooltip listener reads `data-harn-tip`, so there is no per-token JS.
  const tip = classes.some((c) => TIPPABLE.has(c)) ? KEYWORD_DOCS[nodeText(node).trim()] : undefined

  return (
    <span
      key={index}
      className={tip ? [...classes, "harn-tip"].join(" ") : classes.join(" ")}
      data-harn-tip={tip}
    >
      {(node.children ?? []).map(renderHighlightNode)}
    </span>
  )
}

export function highlightHarnSource(source: string) {
  return harnLowlight.highlight("harn", source).children.map(renderHighlightNode)
}
