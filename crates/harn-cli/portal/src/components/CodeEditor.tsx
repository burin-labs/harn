import { useEffect, useMemo, useState } from "react"

import CodeMirror from "@uiw/react-codemirror"
import { StreamLanguage } from "@codemirror/language"
import type { StreamParser } from "@codemirror/language"
import { tags } from "@lezer/highlight"
import { EditorView } from "@codemirror/view"
import { oneDark } from "@codemirror/theme-one-dark"

import { fetchHighlightKeywords } from "../lib/api"
import type { PortalHighlightKeywords } from "../types"

type CodeEditorProps = {
  value: string
  onChange: (value: string) => void
  minHeight?: string
}

const fallbackKeywords: PortalHighlightKeywords = {
  keyword: ["pipeline", "fn", "let", "if", "else", "match", "return", "try", "catch", "throw", "for", "while", "import"],
  literal: ["true", "false", "nil"],
  built_in: ["println", "read_file", "write_file", "workflow_execute", "workflow_graph", "artifact"],
}

function createHarnLanguage(keywordSets: PortalHighlightKeywords) {
  const keywords = new Set(keywordSets.keyword)
  const literals = new Set(keywordSets.literal)
  const builtins = new Set(keywordSets.built_in)

  const parser: StreamParser<unknown> = {
    token(stream) {
      if (stream.eatSpace()) {return null}
      if (stream.match("//")) {
        stream.skipToEnd()
        return "comment"
      }
      if (stream.match("/*")) {
        while (!stream.eol()) {
          if (stream.match("*/", false)) {
            stream.match("*/")
            break
          }
          stream.next()
        }
        return "comment"
      }
      if (stream.peek() === '"') {
        stream.next()
        let escaped = false
        while (!stream.eol()) {
          const ch = stream.next()
          if (escaped) {
            escaped = false
            continue
          }
          if (ch === "\\") {
            escaped = true
            continue
          }
          if (ch === '"') {
            break
          }
        }
        return "string"
      }
      if (stream.match(/\b\d+(?:\.\d+)?(?:ms|s|m|h)\b/)) {return "number"}
      if (stream.match(/\b\d+(?:\.\d+)?(?:[eE][+-]?\d+)?\b/)) {return "number"}
      if (stream.match(/[()[\]{}.,:?]/)) {return null}
      if (stream.match(/[+\-*/%=!<>|&]+/)) {return "operator"}
      const identifier = stream.match(/[A-Za-z_][A-Za-z0-9_]*/)
      if (identifier && identifier !== true) {
        const value = identifier[0]
        if (keywords.has(value)) {return "keyword"}
        if (literals.has(value)) {return "atom"}
        if (builtins.has(value)) {return "builtin"}
        if (/^[A-Z]/.test(value)) {return "typeName"}
        return "variableName"
      }
      stream.next()
      return null
    },
    languageData: {
      commentTokens: { line: "//", block: { open: "/*", close: "*/" } },
    },
    tokenTable: {
      keyword: tags.keyword,
      atom: tags.atom,
      builtin: tags.standard(tags.name),
      typeName: tags.typeName,
      variableName: tags.variableName,
      string: tags.string,
      comment: tags.comment,
      number: tags.number,
      operator: tags.operator,
    },
  }

  return StreamLanguage.define(parser)
}

export function CodeEditor({ value, onChange, minHeight = "280px" }: CodeEditorProps) {
  const [keywordSets, setKeywordSets] = useState<PortalHighlightKeywords>(fallbackKeywords)

  useEffect(() => {
    let cancelled = false

    async function loadKeywords() {
      try {
        const next = await fetchHighlightKeywords()
        if (!cancelled) {
          setKeywordSets(next)
        }
      } catch {
        // Keep fallback highlighting if keyword fetch fails.
      }
    }

    void loadKeywords()
    return () => {
      cancelled = true
    }
  }, [])

  const language = useMemo(() => createHarnLanguage(keywordSets), [keywordSets])
  const extensions = useMemo(() => [language, EditorView.lineWrapping], [language])

  return (
    <CodeMirror
      value={value}
      height={minHeight}
      theme={oneDark}
      extensions={extensions}
      basicSetup={{
        autocompletion: false,
        foldGutter: false,
        highlightActiveLineGutter: true,
      }}
      onChange={onChange}
    />
  )
}
