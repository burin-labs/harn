import { useEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react"
import { useNavigate } from "react-router"
import type { SearchDoc } from "../../vite-plugins/content"
import { useMessages } from "../i18n"

let cachedIndex: SearchDoc[] | null = null

function scoreDoc(doc: SearchDoc, terms: string[]): number {
  const title = doc.title.toLowerCase()
  const headings = doc.headings.join(" ").toLowerCase()
  const text = doc.text.toLowerCase()
  let score = 0
  for (const term of terms) {
    if (title.includes(term)) score += 10
    if (title.startsWith(term)) score += 5
    if (headings.includes(term)) score += 4
    if (text.includes(term)) score += 1
    else if (!title.includes(term) && !headings.includes(term)) return -1
  }
  return score
}

// The flattened body text repeats the page title up front (e.g. "CLI reference
// All commands available…"). Drop that prefix so the result's second line reads
// as a clean description distinct from the bold title above it.
function excerptFor(doc: SearchDoc): string {
  const title = doc.title.trim()
  const text = doc.text.trim()
  if (title && text.toLowerCase().startsWith(title.toLowerCase())) {
    return text.slice(title.length).trimStart()
  }
  return text
}

export function SearchModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const t = useMessages()
  const navigate = useNavigate()
  const [index, setIndex] = useState<SearchDoc[] | null>(cachedIndex)
  const [query, setQuery] = useState("")
  const [active, setActive] = useState(0)
  const inputRef = useRef<HTMLInputElement>(null)

  useEffect(() => {
    if (!open) return
    setQuery("")
    setActive(0)
    inputRef.current?.focus()
    if (!cachedIndex) {
      fetch("/_content/search.json")
        .then((r) => r.json())
        .then((data: SearchDoc[]) => {
          cachedIndex = data
          setIndex(data)
        })
        .catch(() => setIndex([]))
    }
  }, [open])

  const results = useMemo(() => {
    if (!index) return []
    const terms = query.toLowerCase().split(/\s+/).filter(Boolean)
    if (terms.length === 0) return index.slice(0, 8)
    return index
      .map((doc) => ({ doc, score: scoreDoc(doc, terms) }))
      .filter((r) => r.score >= 0)
      .sort((a, b) => b.score - a.score)
      .slice(0, 12)
      .map((r) => r.doc)
  }, [index, query])

  useEffect(() => {
    setActive(0)
  }, [query])

  if (!open) return null

  const go = (url: string) => {
    onClose()
    navigate(url)
  }

  const onKeyDown = (e: ReactKeyboardEvent) => {
    if (e.key === "ArrowDown") {
      e.preventDefault()
      setActive((a) => Math.min(a + 1, results.length - 1))
    } else if (e.key === "ArrowUp") {
      e.preventDefault()
      setActive((a) => Math.max(a - 1, 0))
    } else if (e.key === "Enter") {
      e.preventDefault()
      const target = results[active]
      if (target) go(target.url)
    } else if (e.key === "Escape") {
      onClose()
    }
  }

  return (
    <div
      className="fixed inset-0 z-50 flex items-start justify-center bg-black/40 px-4 pt-[12vh] backdrop-blur-sm"
      onClick={onClose}
    >
      <div
        className="w-full max-w-xl overflow-hidden rounded-2xl border border-border bg-surface-elevated shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center gap-3 border-b border-border px-4">
          <svg
            className="h-5 w-5 shrink-0 text-foreground-muted"
            fill="none"
            viewBox="0 0 24 24"
            stroke="currentColor"
          >
            <path
              strokeLinecap="round"
              strokeLinejoin="round"
              strokeWidth={2}
              d="M21 21l-4.35-4.35M11 19a8 8 0 100-16 8 8 0 000 16z"
            />
          </svg>
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={onKeyDown}
            placeholder={t.search.placeholder}
            className="w-full bg-transparent py-3.5 text-foreground outline-none placeholder:text-foreground-muted"
          />
          <kbd className="rounded border border-border bg-surface-tertiary px-1.5 py-0.5 text-[11px] text-foreground-muted">
            {t.search.esc}
          </kbd>
        </div>
        <ul className="max-h-[55vh] overflow-y-auto p-2">
          {results.length === 0 && (
            <li className="px-3 py-6 text-center text-sm text-foreground-muted">
              {index === null ? t.search.loading : t.search.noResults}
            </li>
          )}
          {results.map((doc, i) => (
            <li key={doc.slug}>
              <button
                onMouseEnter={() => setActive(i)}
                onClick={() => go(doc.url)}
                className={`flex w-full flex-col items-start gap-0.5 rounded-lg px-3 py-2 text-left transition-colors ${
                  i === active ? "bg-accent-500/10" : "hover:bg-surface-tertiary"
                }`}
              >
                <span className="flex items-center gap-2">
                  <span className="text-sm font-medium text-foreground">{doc.title}</span>
                  <span className="rounded-full bg-surface-tertiary px-2 py-0.5 text-[10px] uppercase tracking-wide text-foreground-muted">
                    {doc.sectionTitle}
                  </span>
                </span>
                <span className="line-clamp-1 text-xs text-foreground-muted">{excerptFor(doc)}</span>
              </button>
            </li>
          ))}
        </ul>
      </div>
    </div>
  )
}
