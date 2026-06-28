import { Link } from "react-router"
import { useMessages, type Messages } from "../../i18n"

// Structural data only: the catalog key plus the route each card links to.
const PATH_LINKS: {
  key: keyof Omit<Messages["landing"]["paths"], "sectionTitle" | "sectionBody" | "explore">
  to: string
}[] = [
  { key: "tutorials", to: "/getting-started.html" },
  { key: "guides", to: "/common-tasks.html" },
  { key: "reference", to: "/language-basics.html" },
  { key: "explanation", to: "/host-boundary.html" },
]

export function PathsSection() {
  const p = useMessages().landing.paths
  return (
    <section className="border-t border-border bg-surface-secondary">
      <div className="mx-auto max-w-6xl px-4 py-20 sm:px-6 lg:px-8">
        <div className="mb-12 max-w-2xl">
          <h2 className="text-3xl font-bold tracking-tight text-foreground">{p.sectionTitle}</h2>
          <p className="mt-3 text-foreground-secondary">{p.sectionBody}</p>
        </div>
        <div className="grid gap-5 sm:grid-cols-2 lg:grid-cols-4">
          {PATH_LINKS.map(({ key, to }) => (
            <PathCard
              key={key}
              to={to}
              kicker={p[key].kicker}
              title={p[key].title}
              description={p[key].description}
              cta={p.explore}
            />
          ))}
        </div>
      </div>
    </section>
  )
}

function PathCard({
  title,
  description,
  to,
  kicker,
  cta,
}: {
  title: string
  description: string
  to: string
  kicker: string
  cta: string
}) {
  return (
    <Link
      to={to}
      className="group flex flex-col rounded-xl border border-card-border bg-card-bg p-5 transition-colors hover:border-accent-400"
    >
      <div className="text-[11px] font-semibold uppercase tracking-wider text-accent-600 dark:text-accent-400">
        {kicker}
      </div>
      <h3 className="mt-1 text-base font-semibold text-foreground">{title}</h3>
      <p className="mt-2 flex-1 text-sm leading-relaxed text-foreground-secondary">{description}</p>
      <span className="mt-4 inline-flex items-center gap-1 text-sm font-medium text-accent-700 dark:text-accent-300">
        {cta}
        <svg
          className="h-4 w-4 transition-transform group-hover:translate-x-0.5"
          fill="none"
          viewBox="0 0 24 24"
          stroke="currentColor"
        >
          <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M9 5l7 7-7 7" />
        </svg>
      </span>
    </Link>
  )
}
