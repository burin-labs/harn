import { useState } from "react"
import { exampleScenarios } from "../examples/gallery"
import { highlightHarnSource } from "../lib/harn-highlight"
import { useMessages } from "../i18n"

export { exampleScenarios }

export function ExampleGallery() {
  const t = useMessages()
  const ex = t.landing.examples
  const [activeSlug, setActiveSlug] = useState(exampleScenarios[0]?.slug ?? "review-captain")
  const [copiedSlug, setCopiedSlug] = useState<string | null>(null)
  const active = exampleScenarios.find((scenario) => scenario.slug === activeSlug) ?? exampleScenarios[0]

  async function copyActiveSource() {
    if (!active) return
    if (typeof navigator === "undefined" || !navigator.clipboard) return
    try {
      await navigator.clipboard.writeText(active.source)
      setCopiedSlug(active.slug)
      window.setTimeout(() => setCopiedSlug(null), 1600)
    } catch {
      setCopiedSlug(null)
    }
  }

  if (!active) return null
  const copy = ex.scenarios[active.slug]

  return (
    <div className="overflow-hidden rounded-xl border border-card-border bg-card-bg">
      <div className="border-b border-border bg-surface-secondary px-4 pt-4 sm:px-5">
        <div role="tablist" aria-label={ex.tablistAria} className="flex gap-2 overflow-x-auto pb-4">
          {exampleScenarios.map((scenario) => {
            const isActive = scenario.slug === active.slug
            return (
              <button
                key={scenario.slug}
                type="button"
                role="tab"
                aria-selected={isActive}
                aria-controls={`example-panel-${scenario.slug}`}
                id={`example-tab-${scenario.slug}`}
                onClick={() => setActiveSlug(scenario.slug)}
                className={`h-9 shrink-0 rounded-lg border px-3 text-sm font-semibold transition-colors ${
                  isActive
                    ? "border-accent-500 bg-accent-600 text-white dark:bg-accent-500 dark:text-accent-950"
                    : "border-border-strong bg-card-bg text-foreground-secondary hover:border-accent-400 hover:text-foreground"
                }`}
              >
                {ex.scenarios[scenario.slug].tab}
              </button>
            )
          })}
        </div>
      </div>
      <div
        role="tabpanel"
        id={`example-panel-${active.slug}`}
        aria-labelledby={`example-tab-${active.slug}`}
        className="grid lg:grid-cols-[minmax(0,0.86fr)_minmax(0,1.14fr)]"
      >
        <div className="border-b border-border p-5 lg:border-r lg:border-b-0">
          <h3 className="text-xl font-semibold tracking-tight text-foreground">{copy.title}</h3>
          <p className="mt-3 text-sm leading-relaxed text-foreground-secondary">{copy.outcome}</p>

          {/* The honest "run it" affordance: the exact local command, copyable. */}
          <div className="mt-5 flex items-center gap-2 rounded-lg border border-border bg-surface-tertiary px-3 py-2 font-mono text-xs text-foreground-secondary">
            <span aria-hidden="true" className="select-none text-foreground-muted">
              $
            </span>
            <span className="min-w-0 flex-1 truncate">{active.command}</span>
          </div>

          <div className="mt-5 flex flex-wrap gap-3">
            <a
              href={active.sourceHref}
              target="_blank"
              rel="noopener noreferrer"
              className="inline-flex h-9 items-center justify-center rounded-lg bg-accent-600 px-4 text-sm font-semibold text-white transition-colors hover:bg-accent-700 dark:bg-accent-500 dark:text-accent-950 dark:hover:bg-accent-400"
            >
              {ex.viewSource}
            </a>
            <a
              href={active.docsHref}
              className="inline-flex h-9 items-center justify-center rounded-lg border border-border-strong px-4 text-sm font-semibold text-foreground transition-colors hover:bg-surface-tertiary"
            >
              {ex.readDocs}
            </a>
          </div>
          <div className="mt-6 rounded-lg border border-border bg-surface-tertiary px-3 py-2 font-mono text-xs break-all text-foreground-secondary">
            {active.sourcePath}
          </div>
        </div>
        <div className="min-w-0 bg-[#07100f]">
          <div className="flex items-center justify-between gap-3 border-b border-white/10 bg-[#0c1413] px-4 py-3">
            <span className="font-mono text-xs font-medium text-white/55">{ex.fileLabel}</span>
            <button
              type="button"
              onClick={copyActiveSource}
              aria-label={ex.copyAria}
              className="inline-flex h-8 items-center gap-2 rounded-lg border border-white/10 bg-white/[0.04] px-3 text-xs font-semibold text-white/75 transition-colors hover:bg-white/[0.08] hover:text-white"
            >
              <svg className="h-3.5 w-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
                <path d="M8 8h10v12H8z" />
                <path d="M6 16H4V4h12v2" />
              </svg>
              {copiedSlug === active.slug ? ex.copied : ex.copy}
            </button>
          </div>
          <pre className="m-0 max-h-[30rem] overflow-auto p-4 text-[12px] leading-relaxed whitespace-pre-wrap break-words text-white/80">
            <code data-example-gallery-code="true" className="language-harn">
              {highlightHarnSource(active.source)}
            </code>
          </pre>
        </div>
      </div>
    </div>
  )
}
