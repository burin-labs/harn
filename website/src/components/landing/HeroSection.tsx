import { Link } from "react-router"
import { HarnMockup } from "../HarnMockup"
import { useMessages } from "../../i18n"

const GITHUB = "https://github.com/burin-labs/harn"

export function HeroSection() {
  const t = useMessages()
  const hero = t.landing.hero
  return (
    <section className="relative overflow-hidden border-b border-border">
      {/* Single restrained brand wash, no drifting neon blobs. */}
      <div
        aria-hidden="true"
        className="pointer-events-none absolute inset-x-0 top-0 h-[420px] bg-gradient-to-b from-accent-50/70 to-transparent dark:from-accent-950/30"
      />
      <div className="relative mx-auto max-w-5xl px-4 pt-20 pb-16 sm:px-6 sm:pt-28 lg:px-8">
        <div className="mx-auto max-w-3xl text-center">
          <h1 className="animate-fade-up text-4xl font-bold tracking-tight text-foreground sm:text-5xl lg:text-[3.5rem] lg:leading-[1.07]">
            {hero.headline}
          </h1>
          <p className="mx-auto mt-6 max-w-2xl animate-fade-up-delay-2 text-lg leading-relaxed text-foreground-secondary">
            {hero.subhead}
          </p>
          <div className="mt-9 flex animate-fade-up-delay-3 flex-col items-center gap-3 sm:flex-row sm:justify-center">
            <Link
              to="/getting-started.html"
              className="inline-flex w-full items-center justify-center rounded-lg bg-accent-600 px-6 py-2.5 text-sm font-semibold text-white transition-colors hover:bg-accent-700 sm:w-auto dark:bg-accent-500 dark:hover:bg-accent-400 dark:text-accent-950"
            >
              {hero.getStarted}
            </Link>
            <Link
              to="/introduction.html"
              className="inline-flex w-full items-center justify-center rounded-lg border border-border-strong px-6 py-2.5 text-sm font-semibold text-foreground transition-colors hover:bg-surface-tertiary sm:w-auto"
            >
              {hero.readDocs}
            </Link>
            <a
              href={GITHUB}
              target="_blank"
              rel="noopener noreferrer"
              className="inline-flex items-center justify-center gap-2 px-2 py-2.5 text-sm font-semibold text-foreground-secondary transition-colors hover:text-foreground"
            >
              <svg className="h-4 w-4" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
                <path d="M12 .5C5.73.5.5 5.73.5 12c0 5.08 3.29 9.39 7.86 10.91.58.11.79-.25.79-.56v-2c-3.2.7-3.88-1.54-3.88-1.54-.53-1.34-1.3-1.7-1.3-1.7-1.06-.72.08-.71.08-.71 1.17.08 1.79 1.2 1.79 1.2 1.04 1.79 2.73 1.27 3.4.97.11-.75.41-1.27.74-1.56-2.55-.29-5.23-1.28-5.23-5.69 0-1.26.45-2.29 1.19-3.1-.12-.29-.52-1.46.11-3.05 0 0 .97-.31 3.18 1.18a11.1 11.1 0 015.79 0c2.2-1.49 3.18-1.18 3.18-1.18.63 1.59.23 2.76.11 3.05.74.81 1.19 1.84 1.19 3.1 0 4.42-2.69 5.4-5.25 5.68.42.36.79 1.08.79 2.18v3.23c0 .31.21.68.8.56A11.51 11.51 0 0023.5 12C23.5 5.73 18.27.5 12 .5z" />
              </svg>
              {hero.github}
            </a>
          </div>
        </div>
        <div className="mt-16 animate-fade-up-delay-4">
          <HarnMockup />
        </div>
        <ul className="mx-auto mt-10 flex max-w-3xl flex-wrap items-center justify-center gap-x-6 gap-y-2 text-sm text-foreground-muted">
          {hero.facts.map((fact) => (
            <li key={fact} className="whitespace-nowrap">
              {fact}
            </li>
          ))}
        </ul>
      </div>
    </section>
  )
}
