import { Link } from "react-router"
import { Logo } from "./Logo"
import { ThemeToggle } from "./ThemeToggle"
import { useMessages } from "../i18n"

const GITHUB = "https://github.com/burin-labs/harn"

export function Navbar({ onOpenSearch }: { onOpenSearch: () => void }) {
  const t = useMessages()
  return (
    <nav className="sticky top-0 z-40 border-b border-border bg-nav-bg backdrop-blur-lg">
      <div className="mx-auto flex max-w-6xl items-center justify-between gap-3 px-4 py-3 sm:px-6 lg:px-8">
        <Link to="/" className="flex items-center gap-2" aria-label={t.nav.brandHomeAria}>
          <Logo className="h-8 w-8" />
          <span className="text-lg font-semibold tracking-tight text-foreground">
            {t.common.siteName}
          </span>
        </Link>

        <div className="flex flex-1 items-center justify-end gap-1 sm:gap-2">
          <Link
            to="/introduction.html"
            className="hidden rounded-lg px-3 py-2 text-sm text-foreground-secondary transition-colors hover:text-foreground sm:block"
          >
            {t.nav.docs}
          </Link>
          <Link
            to="/language-basics.html"
            className="hidden rounded-lg px-3 py-2 text-sm text-foreground-secondary transition-colors hover:text-foreground sm:block"
          >
            {t.nav.reference}
          </Link>

          <button
            onClick={onOpenSearch}
            className="flex items-center gap-2 rounded-lg border border-input-border bg-input-bg px-3 py-1.5 text-sm text-foreground-muted transition-colors hover:border-accent-400 hover:text-foreground-secondary"
            aria-label={t.nav.searchAria}
          >
            <svg className="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor">
              <path
                strokeLinecap="round"
                strokeLinejoin="round"
                strokeWidth={2}
                d="M21 21l-4.35-4.35M11 19a8 8 0 100-16 8 8 0 000 16z"
              />
            </svg>
            <span className="hidden md:inline">{t.nav.search}</span>
            <kbd className="hidden rounded border border-border bg-surface-tertiary px-1.5 font-sans text-[11px] text-foreground-muted md:inline">
              {t.nav.cmdK}
            </kbd>
          </button>

          <a
            href={GITHUB}
            target="_blank"
            rel="noopener noreferrer"
            className="rounded-lg p-2 text-foreground-secondary transition-colors hover:bg-surface-tertiary hover:text-foreground"
            aria-label={t.nav.githubAria}
          >
            <svg className="h-5 w-5" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
              <path d="M12 .5C5.73.5.5 5.73.5 12c0 5.08 3.29 9.39 7.86 10.91.58.11.79-.25.79-.56v-2c-3.2.7-3.88-1.54-3.88-1.54-.53-1.34-1.3-1.7-1.3-1.7-1.06-.72.08-.71.08-.71 1.17.08 1.79 1.2 1.79 1.2 1.04 1.79 2.73 1.27 3.4.97.11-.75.41-1.27.74-1.56-2.55-.29-5.23-1.28-5.23-5.69 0-1.26.45-2.29 1.19-3.1-.12-.29-.52-1.46.11-3.05 0 0 .97-.31 3.18 1.18a11.1 11.1 0 015.79 0c2.2-1.49 3.18-1.18 3.18-1.18.63 1.59.23 2.76.11 3.05.74.81 1.19 1.84 1.19 3.1 0 4.42-2.69 5.4-5.25 5.68.42.36.79 1.08.79 2.18v3.23c0 .31.21.68.8.56A11.51 11.51 0 0023.5 12C23.5 5.73 18.27.5 12 .5z" />
            </svg>
          </a>

          <ThemeToggle />
        </div>
      </div>
    </nav>
  )
}
