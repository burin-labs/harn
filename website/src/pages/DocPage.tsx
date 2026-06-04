import { useEffect, useState, type MouseEvent as ReactMouseEvent } from "react"
import { Link, useLocation, useNavigate } from "react-router"
import { nav, meta } from "virtual:harn-docs"
import type { NavItem, NavSection } from "../../vite-plugins/content"
import type { PageData } from "../../vite-plugins/content"
import { pageMetaForDoc } from "../lib/metadata"
import { fetchPage, getCachedPage } from "../lib/page-store"

export function DocPage({ slug }: { slug: string }) {
  const docMeta = meta[slug]
  const [page, setPage] = useState<PageData | null>(() => getCachedPage(slug))
  const [sidebarOpen, setSidebarOpen] = useState(false)
  const navigate = useNavigate()
  const location = useLocation()

  useEffect(() => {
    document.title = pageMetaForDoc(docMeta).title
  }, [docMeta])

  useEffect(() => {
    let cancelled = false
    if (!page || page.slug !== slug) {
      const cached = getCachedPage(slug)
      if (cached) {
        setPage(cached)
      } else {
        void fetchPage(slug).then((data) => {
          if (!cancelled) setPage(data)
        })
      }
    }
    return () => {
      cancelled = true
    }
  }, [slug, page])

  useEffect(() => {
    setSidebarOpen(false)
  }, [location.pathname])

  const section = nav.find((s) => s.id === docMeta.sectionId) ?? nav[0]

  // Intercept clicks on intra-doc links inside the rendered HTML and route them
  // client-side instead of doing a full page load.
  const onContentClick = (e: ReactMouseEvent<HTMLElement>) => {
    const anchor = (e.target as HTMLElement).closest("a")
    if (!anchor) return
    const href = anchor.getAttribute("href") ?? ""
    if (anchor.target === "_blank" || !href.startsWith("/")) return
    e.preventDefault()
    navigate(href)
  }

  return (
    <>
      {/* Full-width section tabs (the top-level Diataxis switcher). */}
      <div className="sticky top-14 z-20 border-b border-border bg-nav-bg backdrop-blur-lg">
        <div className="mx-auto max-w-7xl px-4 sm:px-6 lg:px-8">
          <SectionTabs sections={nav} activeId={section.id} />
        </div>
      </div>

      <div className="mx-auto flex max-w-7xl gap-0 px-0 sm:px-4 lg:px-8">
      {/* Sidebar */}
      <aside
        className={`fixed inset-0 z-30 bg-black/40 backdrop-blur-sm lg:static lg:z-auto lg:block lg:bg-transparent lg:backdrop-blur-none ${
          sidebarOpen ? "block" : "hidden"
        }`}
        onClick={() => setSidebarOpen(false)}
      >
        <div
          className="docs-scroll h-full w-72 max-w-[80vw] overflow-y-auto border-r border-border bg-surface px-4 pt-6 pb-16 lg:sticky lg:top-[6.75rem] lg:h-[calc(100vh-6.75rem)] lg:w-64 lg:border-r-0 lg:bg-transparent lg:px-2"
          onClick={(e) => e.stopPropagation()}
        >
          <SidebarNav section={section} currentSlug={slug} />
        </div>
      </aside>

      {/* Content + TOC */}
      <div className="flex min-w-0 flex-1 justify-center gap-10">
        <article className="min-w-0 flex-1 px-4 py-8 sm:px-6 lg:max-w-3xl lg:px-10">
          <button
            onClick={() => setSidebarOpen(true)}
            className="mb-4 inline-flex items-center gap-2 rounded-lg border border-border px-3 py-1.5 text-sm text-foreground-secondary lg:hidden"
          >
            <svg className="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor">
              <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M4 6h16M4 12h16M4 18h16" />
            </svg>
            {section.title}
          </button>

          <nav className="mb-4 flex items-center gap-1.5 text-xs text-foreground-muted">
            <Link to={section.url} className="hover:text-foreground-secondary">
              {section.title}
            </Link>
            <span>/</span>
            <span className="text-foreground-secondary">
              <RichTitle text={docMeta.navTitle} />
            </span>
          </nav>

          {page ? (
            <div
              className="prose max-w-none"
              onClick={onContentClick}
              dangerouslySetInnerHTML={{ __html: page.html }}
            />
          ) : (
            <div className="prose max-w-none">
              <h1>{docMeta.title}</h1>
              <p className="text-foreground-muted">Loading…</p>
            </div>
          )}

          {page && (page.prev || page.next) && (
            <div className="mt-12 grid gap-4 border-t border-border pt-6 sm:grid-cols-2">
              {page.prev ? (
                <Link
                  to={page.prev.url}
                  className="group rounded-xl border border-card-border bg-card-bg p-4 transition-colors hover:border-accent-400"
                >
                  <div className="text-xs text-foreground-muted">Previous</div>
                  <div className="mt-1 font-medium text-foreground group-hover:text-accent-600 dark:group-hover:text-accent-300">
                    <RichTitle text={page.prev.title} />
                  </div>
                </Link>
              ) : (
                <span />
              )}
              {page.next && (
                <Link
                  to={page.next.url}
                  className="group rounded-xl border border-card-border bg-card-bg p-4 text-right transition-colors hover:border-accent-400 sm:col-start-2"
                >
                  <div className="text-xs text-foreground-muted">Next</div>
                  <div className="mt-1 font-medium text-foreground group-hover:text-accent-600 dark:group-hover:text-accent-300">
                    <RichTitle text={page.next.title} />
                  </div>
                </Link>
              )}
            </div>
          )}

          <div className="mt-10 border-t border-border pt-6 text-sm">
            <a
              href={docMeta.editUrl}
              target="_blank"
              rel="noopener noreferrer"
              className="inline-flex items-center gap-1.5 text-foreground-muted transition-colors hover:text-accent-600 dark:hover:text-accent-300"
            >
              <svg className="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                <path
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  strokeWidth={2}
                  d="M11 5H6a2 2 0 00-2 2v11a2 2 0 002 2h11a2 2 0 002-2v-5m-1.414-9.414a2 2 0 112.828 2.828L11.828 15H9v-2.828l8.586-8.586z"
                />
              </svg>
              Edit this page on GitHub
            </a>
          </div>
        </article>

        {page && page.headings.length > 0 && (
          <Toc headings={page.headings} pathname={location.pathname} />
        )}
      </div>
      </div>
    </>
  )
}

// Renders a nav/title string with `backtick` spans as inline monospace code.
function RichTitle({ text }: { text: string }) {
  if (!text.includes("`")) return <>{text}</>
  const parts = text.split(/(`[^`]+`)/g)
  return (
    <>
      {parts.map((part, i) =>
        part.startsWith("`") && part.endsWith("`") && part.length > 1 ? (
          <code
            key={i}
            className="rounded bg-surface-tertiary px-1 py-0.5 font-mono text-[0.85em]"
          >
            {part.slice(1, -1)}
          </code>
        ) : (
          <span key={i}>{part}</span>
        ),
      )}
    </>
  )
}

function SectionTabs({ sections, activeId }: { sections: NavSection[]; activeId: string }) {
  return (
    <nav className="no-scrollbar flex gap-1 overflow-x-auto" aria-label="Documentation sections">
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

function SidebarNav({ section, currentSlug }: { section: NavSection; currentSlug: string }) {
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
            <SidebarItem
              key={child.slug}
              item={child}
              currentSlug={currentSlug}
              depth={depth + 1}
            />
          ))}
        </ul>
      )}
    </li>
  )
}

function Toc({
  headings,
  pathname,
}: {
  headings: PageData["headings"]
  pathname: string
}) {
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
          On this page
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
