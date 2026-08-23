import { useEffect, type RefObject } from "react"
import { renderDiagrams } from "../lib/mermaid"

/**
 * Render the Mermaid figures inside `ref` whenever the page content changes,
 * and re-render them when the theme is toggled.
 *
 * The theme is observed from the `.dark` class on <html> rather than taken from
 * `useTheme`, because the toggle that owns that class lives in the navbar and
 * each `useTheme()` call keeps its own state.
 */
export function useMermaid(ref: RefObject<HTMLElement | null>, contentKey: string | null) {
  useEffect(() => {
    const root = ref.current
    if (!root || contentKey === null) return

    let cancelled = false
    const render = (force: boolean) => {
      if (cancelled) return
      void renderDiagrams(root, force)
    }

    render(false)

    const observer = new MutationObserver(() => render(true))
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["class"],
    })
    return () => {
      cancelled = true
      observer.disconnect()
    }
  }, [ref, contentKey])
}
