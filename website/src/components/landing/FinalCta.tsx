import { Link } from "react-router"
import { useMessages } from "../../i18n"

export function FinalCta() {
  const cta = useMessages().landing.cta
  return (
    <section className="border-t border-border">
      <div className="mx-auto max-w-5xl px-4 py-20 text-center sm:px-6 lg:px-8">
        <h2 className="text-2xl font-bold tracking-tight text-foreground sm:text-3xl">{cta.title}</h2>
        <p className="mx-auto mt-3 max-w-xl text-foreground-secondary">{cta.body}</p>
        <div className="mt-8 flex flex-col items-center gap-3 sm:flex-row sm:justify-center">
          <Link
            to="/getting-started.html"
            className="inline-flex items-center justify-center rounded-lg bg-accent-600 px-6 py-2.5 text-sm font-semibold text-white transition-colors hover:bg-accent-700 dark:bg-accent-500 dark:hover:bg-accent-400 dark:text-accent-950"
          >
            {cta.getStarted}
          </Link>
          <Link
            to="/cookbook.html"
            className="inline-flex items-center justify-center rounded-lg border border-border-strong px-6 py-2.5 text-sm font-semibold text-foreground transition-colors hover:bg-surface-tertiary"
          >
            {cta.browseCookbook}
          </Link>
        </div>
      </div>
    </section>
  )
}
