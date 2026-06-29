import { useMessages } from "../i18n"

const RELEASES = "https://github.com/burin-labs/harn/releases"

// A thin site-wide notice that Harn is pre-1.0. Rendered above the navbar on
// every page (landing + docs) so the "subject to change" caveat travels with
// the content and points readers at the release notes for what moved.
export function PrereleaseBanner() {
  const t = useMessages()
  return (
    <div className="border-b border-accent-500/20 bg-accent-500/10">
      <p className="mx-auto max-w-6xl px-4 py-1.5 text-center text-xs text-foreground-secondary sm:px-6 lg:px-8">
        <span className="font-semibold text-accent-700 dark:text-accent-300">
          {t.banner.label}
        </span>{" "}
        {t.banner.prerelease}{" "}
        <a
          href={RELEASES}
          target="_blank"
          rel="noopener noreferrer"
          className="font-medium text-accent-700 underline decoration-accent-500/40 underline-offset-2 transition-colors hover:text-accent-600 dark:text-accent-300 dark:hover:text-accent-200"
        >
          {t.banner.changelogLink}
        </a>
      </p>
    </div>
  )
}
