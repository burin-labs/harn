import { useEffect, useState } from "react"
import type { PageData } from "../../../vite-plugins/content"
import { useMessages } from "../../i18n"

// The "On this page" rail; highlights the heading currently in view.
export function Toc({ headings, pathname }: { headings: PageData["headings"]; pathname: string }) {
  const t = useMessages()
  const [activeId, setActiveId] = useState<string>("")

  useEffect(() => {
    const els = headings
      .map((h) => document.getElementById(h.id))
      .filter((el): el is HTMLElement => el !== null)
    if (els.length === 0) return
    const observer = new IntersectionObserver(
      (entries) => {
        const visible = entries.filter((e) => e.isIntersecting)
        if (visible.length > 0) setActiveId(visible[0].target.id)
      },
      { rootMargin: "-80px 0px -70% 0px", threshold: 0 },
    )
    els.forEach((el) => observer.observe(el))
    return () => observer.disconnect()
  }, [headings, pathname])

  return (
    <aside className="hidden w-56 shrink-0 xl:block">
      <div className="docs-scroll sticky top-[6.75rem] max-h-[calc(100vh-8rem)] overflow-y-auto py-8">
        <div className="mb-2 text-[11px] font-semibold uppercase tracking-wider text-foreground-muted">
          {t.docs.onThisPage}
        </div>
        <ul className="space-y-1 border-l border-border">
          {headings.map((h) => (
            <li key={h.id}>
              <a
                href={`#${h.id}`}
                className={`block border-l-2 py-0.5 text-sm transition-colors ${
                  h.depth === 3 ? "pl-6" : "pl-3"
                } ${
                  activeId === h.id
                    ? "border-accent-500 text-accent-700 dark:text-accent-300"
                    : "border-transparent text-foreground-muted hover:text-foreground-secondary"
                }`}
              >
                {h.text}
              </a>
            </li>
          ))}
        </ul>
      </div>
    </aside>
  )
}
