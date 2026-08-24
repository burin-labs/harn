import { useEffect, useRef, useState, type MouseEvent as ReactMouseEvent } from "react"
import { Link, useLocation, useNavigate } from "react-router"
import { nav, meta } from "virtual:harn-docs"
import type { PageData } from "../../vite-plugins/content"
import { pageMetaForDoc } from "../lib/metadata"
import { fetchPage, getCachedPage } from "../lib/page-store"
import { useMermaid } from "../hooks/useMermaid"
import { useComparisonMatrix } from "../hooks/useComparisonMatrix"
import { useMessages } from "../i18n"
import { plainTitle, RichTitle } from "../components/docs/RichTitle"
import { SectionTabs } from "../components/docs/SectionTabs"
import { SidebarNav } from "../components/docs/SidebarNav"
import { Toc } from "../components/docs/Toc"

export function DocPage({ slug }: { slug: string }) {
  const t = useMessages()
  const docMeta = meta[slug]
  const [page, setPage] = useState<PageData | null>(() => getCachedPage(slug))
  const [sidebarOpen, setSidebarOpen] = useState(false)
  const navigate = useNavigate()
  const location = useLocation()
  const contentRef = useRef<HTMLDivElement>(null)

  useMermaid(contentRef, page?.slug ?? null)
  useComparisonMatrix(contentRef, page?.slug ?? null)

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
        <div className="mx-auto max-w-[84rem] px-4 sm:px-6 lg:px-8">
          <SectionTabs sections={nav} activeId={section.id} />
        </div>
      </div>

      <div className="mx-auto flex max-w-[84rem] gap-0 px-0 sm:px-4 lg:px-8">
        {/* Sidebar */}
        <aside
          className={`fixed inset-0 z-30 bg-black/40 backdrop-blur-sm lg:static lg:z-auto lg:block lg:bg-transparent lg:backdrop-blur-none ${
            sidebarOpen ? "block" : "hidden"
          }`}
          onClick={() => setSidebarOpen(false)}
        >
          <div
            className="docs-scroll h-full w-72 max-w-[80vw] overflow-y-auto border-r border-border bg-surface px-4 pt-6 pb-16 lg:sticky lg:top-[6.75rem] lg:h-[calc(100vh-6.75rem)] lg:w-64 lg:bg-transparent lg:px-2 lg:pr-4"
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

            <nav
              className="mb-4 flex items-center gap-1.5 text-xs text-foreground-muted"
              aria-label={t.docs.breadcrumbAria}
            >
              <Link to={section.url} className="hover:text-foreground-secondary">
                {section.title}
              </Link>
              <span aria-hidden="true">/</span>
              <span className="text-foreground-secondary">
                <RichTitle text={docMeta.navTitle} />
              </span>
            </nav>

            {page ? (
              <div
                ref={contentRef}
                className="prose max-w-none"
                onClick={onContentClick}
                dangerouslySetInnerHTML={{ __html: page.html }}
              />
            ) : (
              <div className="prose max-w-none">
                <h1>{docMeta.title}</h1>
                <p className="text-foreground-muted">{t.search.loading}</p>
              </div>
            )}

            {page && (page.prev || page.next) && (
              /* Two text links on one rule. The direction is carried by the
                 arrow for sighted readers and by the label for everyone else,
                 so neither needs a card around it. */
              <nav
                aria-label={t.docs.pagerAria}
                className="mt-12 flex items-start justify-between gap-8 border-t border-border pt-6 text-sm"
              >
                {page.prev ? (
                  <Link
                    to={page.prev.url}
                    aria-label={`${t.docs.previous}: ${plainTitle(page.prev.title)}`}
                    className="group inline-flex max-w-[46%] items-baseline gap-1.5 text-foreground-secondary transition-colors hover:text-accent-700 dark:hover:text-accent-300"
                  >
                    <span aria-hidden="true" className="text-foreground-muted transition-transform group-hover:-translate-x-0.5">
                      &larr;
                    </span>
                    <span className="link-underline">
                      <RichTitle text={page.prev.title} />
                    </span>
                  </Link>
                ) : (
                  <span />
                )}
                {page.next && (
                  <Link
                    to={page.next.url}
                    aria-label={`${t.docs.next}: ${plainTitle(page.next.title)}`}
                    className="group inline-flex max-w-[46%] items-baseline gap-1.5 text-right text-foreground-secondary transition-colors hover:text-accent-700 dark:hover:text-accent-300"
                  >
                    <span className="link-underline">
                      <RichTitle text={page.next.title} />
                    </span>
                    <span aria-hidden="true" className="text-foreground-muted transition-transform group-hover:translate-x-0.5">
                      &rarr;
                    </span>
                  </Link>
                )}
              </nav>
            )}

            <div className="mt-6 border-t border-border pt-6 text-sm">
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
                {t.docs.editOnGitHub}
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
