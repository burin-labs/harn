import { useEffect, type RefObject } from "react"

// Progressive enhancement for the comparison table on the feature matrix page.
//
// The table ships complete: every column is in the prerendered HTML, so a
// reader with JavaScript off, a crawler, and the Markdown projection an agent
// reads all see the whole comparison. This hook only narrows what is on screen
// to the columns a reader asked for, which is what keeps eight systems from
// becoming eight columns of horizontal scroll.
//
// The choice is remembered per browser. It is a display preference, so losing
// it costs the reader nothing and every access is guarded.

const STORAGE_KEY = "harn:comparison-columns"

function readStored(): string[] | null {
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY)
    if (!raw) return null
    const parsed: unknown = JSON.parse(raw)
    return Array.isArray(parsed) && parsed.every((v) => typeof v === "string") ? parsed : null
  } catch {
    return null
  }
}

function writeStored(ids: string[]): void {
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(ids))
  } catch {
    /* private mode, blocked site data — the picker still works this session */
  }
}

export function useComparisonMatrix(
  containerRef: RefObject<HTMLElement | null>,
  slug: string | null,
): void {
  useEffect(() => {
    const root = containerRef.current
    if (!root) return

    const legend = root.querySelector<HTMLElement>("[data-comparison-legend]")
    if (!legend) return

    const boxes = Array.from(legend.querySelectorAll<HTMLInputElement>('input[type="checkbox"]'))
    if (boxes.length === 0) return

    // The legend is inert without this hook, so it stays hidden until now.
    legend.hidden = false

    const apply = () => {
      for (const box of boxes) {
        const shown = box.checked
        const cells = root.querySelectorAll<HTMLElement>(`[data-system="${CSS.escape(box.value)}"]`)
        for (const cell of cells) cell.hidden = !shown
      }
    }

    const stored = readStored()
    if (stored) {
      for (const box of boxes) box.checked = stored.includes(box.value)
    }
    apply()

    const onChange = () => {
      apply()
      writeStored(boxes.filter((b) => b.checked).map((b) => b.value))
    }

    for (const box of boxes) box.addEventListener("change", onChange)
    return () => {
      for (const box of boxes) box.removeEventListener("change", onChange)
    }
  }, [containerRef, slug])
}
