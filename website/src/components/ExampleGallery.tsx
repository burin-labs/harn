import { useState } from "react"
import { exampleScenarios, type ExampleFile } from "../examples/gallery"
import { highlightHarnSource, highlightPromptTemplate } from "../lib/harn-highlight"
import { useMessages } from "../i18n"

export { exampleScenarios }

function highlightFile(file: ExampleFile) {
  return file.lang === "prompt" ? highlightPromptTemplate(file.source) : highlightHarnSource(file.source)
}

export function ExampleGallery() {
  const t = useMessages()
  const ex = t.landing.examples
  const [activeSlug, setActiveSlug] = useState(exampleScenarios[0]?.slug ?? "review-captain")
  const [activeFileName, setActiveFileName] = useState(exampleScenarios[0]?.files[0]?.name ?? "")
  const [copiedKey, setCopiedKey] = useState<string | null>(null)
  const active = exampleScenarios.find((scenario) => scenario.slug === activeSlug) ?? exampleScenarios[0]

  function selectScenario(slug: typeof activeSlug) {
    const scenario = exampleScenarios.find((s) => s.slug === slug)
    setActiveSlug(slug)
    setActiveFileName(scenario?.files[0]?.name ?? "")
  }

  if (!active) return null
  const activeFile = active.files.find((file) => file.name === activeFileName) ?? active.files[0]
  if (!activeFile) return null
  const copyKey = `${active.slug}:${activeFile.name}`
  const copy = ex.scenarios[active.slug]
  const isMultiFile = active.files.length > 1

  async function copyActiveSource() {
    if (typeof navigator === "undefined" || !navigator.clipboard) return
    try {
      await navigator.clipboard.writeText(activeFile.source)
      setCopiedKey(copyKey)
      window.setTimeout(() => setCopiedKey(null), 1600)
    } catch {
      setCopiedKey(null)
    }
  }

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
                data-scenario-tab="true"
                aria-selected={isActive}
                aria-controls={`example-panel-${scenario.slug}`}
                id={`example-tab-${scenario.slug}`}
                onClick={() => selectScenario(scenario.slug)}
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
              href={activeFile.href}
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
          {isMultiFile && <p className="mt-5 text-xs leading-relaxed text-foreground-muted">{ex.multiFileNote}</p>}
          <div className="mt-6 rounded-lg border border-border bg-surface-tertiary px-3 py-2 font-mono text-xs break-all text-foreground-secondary">
            {activeFile.path}
          </div>
        </div>
        <div className="min-w-0 bg-[#07100f]">
          <div className="flex items-center justify-between gap-3 border-b border-white/10 bg-[#0c1413] px-2 py-2 pl-3">
            {isMultiFile ? (
              <div role="tablist" aria-label={ex.filesAria} className="flex min-w-0 gap-1 overflow-x-auto">
                {active.files.map((file) => {
                  const isActive = file.name === activeFile.name
                  return (
                    <button
                      key={file.name}
                      type="button"
                      role="tab"
                      data-file-tab="true"
                      aria-selected={isActive}
                      onClick={() => setActiveFileName(file.name)}
                      className={`h-7 shrink-0 rounded-md px-2.5 font-mono text-xs font-medium transition-colors ${
                        isActive ? "bg-white/[0.12] text-white" : "text-white/45 hover:bg-white/[0.06] hover:text-white/80"
                      }`}
                    >
                      {file.name}
                    </button>
                  )
                })}
              </div>
            ) : (
              <span className="px-1 font-mono text-xs font-medium text-white/55">{activeFile.name}</span>
            )}
            <button
              type="button"
              onClick={copyActiveSource}
              aria-label={ex.copyAria}
              className="inline-flex h-8 shrink-0 items-center gap-2 rounded-lg border border-white/10 bg-white/[0.04] px-3 text-xs font-semibold text-white/75 transition-colors hover:bg-white/[0.08] hover:text-white"
            >
              <svg className="h-3.5 w-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
                <path d="M8 8h10v12H8z" />
                <path d="M6 16H4V4h12v2" />
              </svg>
              {copiedKey === copyKey ? ex.copied : ex.copy}
            </button>
          </div>
          <pre className="m-0 max-h-[30rem] overflow-auto p-4 text-[12px] leading-relaxed whitespace-pre-wrap break-words text-white/80">
            <code data-example-gallery-code="true" className={`language-${activeFile.lang === "prompt" ? "prompt" : "harn"}`}>
              {highlightFile(activeFile)}
            </code>
          </pre>
        </div>
      </div>
    </div>
  )
}
