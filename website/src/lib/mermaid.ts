// Client-side Mermaid rendering for the `figure.mermaid-figure` markup the
// build-time pipeline emits (see vite-plugins/content.ts).
//
// Mermaid is loaded with a dynamic import so it lands in its own chunk and is
// fetched only on the few documentation pages that actually contain a diagram.
// Everything the renderer needs is already in the DOM: the diagram source lives
// in the figure's collapsed <details>, and the palette comes from the same CSS
// custom properties the rest of the site is styled with, so the diagrams follow
// the theme toggle without a second copy of the brand colors.

import {
  DIAGRAM_CANVAS_CLASS,
  DIAGRAM_FIGURE_CLASS,
  DIAGRAM_SOURCE_CLASS,
  type DiagramState,
} from "./diagram-markup"

let counter = 0

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type MermaidApi = typeof import("mermaid")["default"]

let mermaidPromise: Promise<MermaidApi> | null = null

function loadMermaid(): Promise<MermaidApi> {
  mermaidPromise ??= import("mermaid").then((m) => m.default)
  return mermaidPromise
}

function cssVar(styles: CSSStyleDeclaration, name: string, fallback: string): string {
  return styles.getPropertyValue(name).trim() || fallback
}

function themeVariables(dark: boolean) {
  const styles = getComputedStyle(document.documentElement)
  const surface = cssVar(styles, "--color-surface", dark ? "#07100f" : "#ffffff")
  const line = cssVar(styles, "--color-border-strong", dark ? "#2b3d3a" : "#ccd8d5")
  const text = cssVar(styles, "--color-foreground", dark ? "#eef4f2" : "#0c1413")
  const accent = cssVar(styles, dark ? "--color-accent-400" : "--color-accent-600", "#0d9488")
  const nodeFill = cssVar(
    styles,
    dark ? "--color-surface-secondary" : "--color-surface-tertiary",
    dark ? "#0d1817" : "#edf4f2",
  )
  const font = cssVar(styles, "--font-sans", "system-ui, sans-serif")

  return {
    background: surface,
    fontFamily: font,
    fontSize: "14px",
    primaryColor: nodeFill,
    primaryTextColor: text,
    primaryBorderColor: accent,
    secondaryColor: nodeFill,
    secondaryTextColor: text,
    secondaryBorderColor: line,
    tertiaryColor: surface,
    tertiaryTextColor: text,
    tertiaryBorderColor: line,
    lineColor: accent,
    textColor: text,
    mainBkg: nodeFill,
    nodeBorder: accent,
    clusterBkg: surface,
    clusterBorder: line,
    edgeLabelBackground: surface,
    titleColor: text,
    noteBkgColor: nodeFill,
    noteTextColor: text,
    noteBorderColor: line,
    actorBkg: nodeFill,
    actorBorder: accent,
    actorTextColor: text,
    signalColor: text,
    signalTextColor: text,
    labelBoxBkgColor: nodeFill,
    labelBoxBorderColor: accent,
    labelTextColor: text,
    loopTextColor: text,
    sequenceNumberColor: surface,
  }
}

/**
 * Render every not-yet-rendered diagram inside `root`, or re-render all of them
 * when `force` is set (the theme changed). Resolves once the pass is done; a
 * root with no diagrams resolves without importing Mermaid at all.
 */
export async function renderDiagrams(root: ParentNode, force = false): Promise<void> {
  const setState = (figure: HTMLElement, state: DiagramState) => {
    figure.dataset.mermaidState = state
  }
  const figures = Array.from(
    root.querySelectorAll<HTMLElement>(`figure.${DIAGRAM_FIGURE_CLASS}`),
  ).filter((figure) => force || figure.dataset.mermaidState !== "ready")
  if (figures.length === 0) return

  const dark = document.documentElement.classList.contains("dark")

  let mermaid: MermaidApi
  try {
    mermaid = await loadMermaid()
  } catch {
    // Offline or a blocked chunk: the <details> source stays as the fallback.
    for (const figure of figures) setState(figure, "unavailable")
    return
  }

  mermaid.initialize({
    startOnLoad: false,
    // `strict` keeps Mermaid's HTML sanitizer on. Diagram sources come from the
    // repository, but the renderer should not be the thing that trusts them.
    securityLevel: "strict",
    theme: "base",
    themeVariables: themeVariables(dark),
    flowchart: { curve: "basis", useMaxWidth: true },
    sequence: { useMaxWidth: true },
  })

  for (const figure of figures) {
    const canvas = figure.querySelector<HTMLElement>(`.${DIAGRAM_CANVAS_CLASS}`)
    const source = figure.querySelector<HTMLElement>(
      `.${DIAGRAM_SOURCE_CLASS} code`,
    )?.textContent
    if (!canvas || !source) continue
    try {
      const { svg } = await mermaid.render(`mermaid-${++counter}`, source)
      canvas.innerHTML = svg
      setState(figure, "ready")
    } catch (error) {
      // A diagram that will not parse should be loud in development and
      // harmless in production: the source stays visible either way.
      console.error("mermaid: failed to render a diagram", error)
      setState(figure, "failed")
    }
  }
}
