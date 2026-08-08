import { Link } from "react-router"
import type { NavItem, NavSection } from "../../../vite-plugins/content"
import { RichTitle } from "./RichTitle"

// The per-section sidebar: grouped, nestable page links.
export function SidebarNav({ section, currentSlug }: { section: NavSection; currentSlug: string }) {
  return (
    <nav className="space-y-5">
      {section.groups.map((group, i) => (
        <div key={i}>
          {group.heading && (
            <div className="mb-1.5 px-2 text-[11px] font-semibold uppercase tracking-wider text-foreground-muted">
              {group.heading}
            </div>
          )}
          <ul className="space-y-0.5">
            {group.items.map((item) => (
              <SidebarItem key={item.slug} item={item} currentSlug={currentSlug} depth={0} />
            ))}
          </ul>
        </div>
      ))}
    </nav>
  )
}

function SidebarItem({
  item,
  currentSlug,
  depth,
}: {
  item: NavItem
  currentSlug: string
  depth: number
}) {
  const active = item.slug === currentSlug
  return (
    <li>
      <Link
        to={item.url}
        className={`block rounded-md px-2 py-1 text-sm transition-colors ${
          active
            ? "bg-accent-500/10 font-medium text-accent-700 dark:text-accent-300"
            : "text-foreground-secondary hover:bg-surface-tertiary hover:text-foreground"
        }`}
        style={{ paddingLeft: `${0.5 + depth * 0.75}rem` }}
      >
        <RichTitle text={item.title} />
      </Link>
      {item.children.length > 0 && (
        <ul className="mt-0.5 space-y-0.5">
          {item.children.map((child) => (
            <SidebarItem key={child.slug} item={child} currentSlug={currentSlug} depth={depth + 1} />
          ))}
        </ul>
      )}
    </li>
  )
}
