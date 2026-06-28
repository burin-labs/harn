import type { ReactNode } from "react"
import { useMessages, type Messages } from "../../i18n"
import { Icon } from "./Icon"

// Structural data only: the icon plus the catalog key whose copy each card shows.
const FEATURE_ICONS: {
  key: keyof Omit<Messages["landing"]["features"], "sectionTitle" | "sectionBody">
  icon: ReactNode
}[] = [
  { key: "pipelines", icon: <Icon d="M5 7H4a2 2 0 00-2 2v6a2 2 0 002 2h1m14 0h1a2 2 0 002-2V9a2 2 0 00-2-2h-1M9 12h6m0 0l-2-2m2 2l-2 2" /> },
  { key: "llms", icon: <Icon d="M9 3v2m6-2v2M9 19v2m6-2v2M5 9H3m2 6H3m18-6h-2m2 6h-2M7 7h10v10H7z" /> },
  { key: "capabilities", icon: <Icon d="M12 3l7 3v5c0 4.4-3 8.2-7 10-4-1.8-7-5.6-7-10V6l7-3z" /> },
  { key: "replay", icon: <Icon d="M3 12a9 9 0 109-9 9 9 0 00-7 3.3M3 4v3.3h3.3M12 8v4l3 2" /> },
  { key: "durable", icon: <Icon d="M4 7h16M4 12h16M4 17h10M19 15l2 2-2 2" /> },
  { key: "protocols", icon: <Icon d="M14 7l3-3a3 3 0 014 4l-3 3m-9 9l-3 3a3 3 0 01-4-4l3-3m1-5l6 6" /> },
]

export function FeaturesSection() {
  const f = useMessages().landing.features
  return (
    <section className="mx-auto max-w-6xl px-4 py-20 sm:px-6 lg:px-8">
      <div className="mb-12 max-w-2xl">
        <h2 className="text-3xl font-bold tracking-tight text-foreground">{f.sectionTitle}</h2>
        <p className="mt-3 text-foreground-secondary">{f.sectionBody}</p>
      </div>
      <div className="grid gap-px overflow-hidden rounded-xl border border-card-border bg-border sm:grid-cols-2 lg:grid-cols-3">
        {FEATURE_ICONS.map(({ key, icon }) => (
          <FeatureCard key={key} icon={icon} title={f[key].title} description={f[key].description} />
        ))}
      </div>
    </section>
  )
}

function FeatureCard({
  icon,
  title,
  description,
}: {
  icon: ReactNode
  title: string
  description: string
}) {
  return (
    <div className="bg-card-bg p-6">
      <div className="mb-3 text-accent-600 dark:text-accent-400">{icon}</div>
      <h3 className="mb-1.5 text-base font-semibold text-foreground">{title}</h3>
      <p className="text-sm leading-relaxed text-foreground-secondary">{description}</p>
    </div>
  )
}
