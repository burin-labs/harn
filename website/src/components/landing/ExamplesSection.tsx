import { ExampleGallery } from "../ExampleGallery"
import { useMessages } from "../../i18n"

export function ExamplesSection() {
  const ex = useMessages().landing.examples
  return (
    <section className="border-b border-border bg-surface-secondary">
      <div className="mx-auto max-w-6xl px-4 py-20 sm:px-6 lg:px-8">
        <div className="mb-10 max-w-2xl">
          <h2 className="text-3xl font-bold tracking-tight text-foreground">{ex.sectionTitle}</h2>
          <p className="mt-3 text-foreground-secondary">{ex.sectionBody}</p>
        </div>
        <ExampleGallery />
      </div>
    </section>
  )
}
