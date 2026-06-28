import { Link } from "react-router"
import type { NavSection } from "../../../vite-plugins/content"
import { useMessages } from "../../i18n"

// The top-level Diátaxis switcher (Introduction, Tutorials, Guides, …).
export function SectionTabs({ sections, activeId }: { sections: NavSection[]; activeId: string }) {
  const t = useMessages()
  return (
    <nav className="no-scrollbar flex gap-1 overflow-x-auto" aria-label={t.docs.sectionsAria}>
      {sections.map((s) => (
        <Link
          key={s.id}
          to={s.url}
          className={`shrink-0 border-b-2 px-3 py-3 text-sm font-medium transition-colors ${
            s.id === activeId
              ? "border-accent-500 text-foreground"
              : "border-transparent text-foreground-muted hover:border-border-strong hover:text-foreground-secondary"
          }`}
        >
          {s.title}
        </Link>
      ))}
    </nav>
  )
}
