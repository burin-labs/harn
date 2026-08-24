import { useEffect } from "react"

import {
  HEADING_ANCHOR_CLASS,
  HEADING_ANCHOR_COPIED_ATTR,
} from "../lib/heading-markup.ts"

const CONFIRMATION_MS = 1200

/**
 * Copy a heading's absolute URL when its permalink is clicked.
 *
 * Progressive enhancement only. The anchor is a real `<a href="#id">` emitted
 * at build time, so with this hook absent — or with the Clipboard API refused,
 * which is what an insecure origin or a denied permission looks like — the
 * click still scrolls and still puts the fragment in the address bar.
 *
 * The default is deliberately not prevented. Copying is the extra; navigating
 * is the contract, and swallowing the event to copy would trade a behaviour
 * every reader expects for one only some of them want.
 */
export function useHeadingAnchors(
  containerRef: React.RefObject<HTMLElement | null>,
  slug: string | null,
): void {
  useEffect(() => {
    const container = containerRef.current
    if (!container || slug === null) return

    let timer: number | undefined

    const onClick = (event: MouseEvent) => {
      const target = event.target as HTMLElement | null
      const anchor = target?.closest?.(`a.${HEADING_ANCHOR_CLASS}`)
      if (!(anchor instanceof HTMLAnchorElement)) return

      // A modified click is the reader opening the section in a new tab or
      // window. Copying then would be a side effect they did not ask for.
      if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return

      // `anchor.href` is already absolute, and resolves against the real page
      // URL rather than anything this module has to reconstruct.
      void navigator.clipboard
        ?.writeText(anchor.href)
        .then(() => {
          anchor.setAttribute(HEADING_ANCHOR_COPIED_ATTR, "")
          window.clearTimeout(timer)
          timer = window.setTimeout(() => {
            anchor.removeAttribute(HEADING_ANCHOR_COPIED_ATTR)
          }, CONFIRMATION_MS)
        })
        .catch(() => {
          // Clipboard refused. The navigation already happened, so there is
          // nothing to recover and nothing worth interrupting the reader over.
        })
    }

    container.addEventListener("click", onClick)
    return () => {
      container.removeEventListener("click", onClick)
      window.clearTimeout(timer)
    }
  }, [containerRef, slug])
}
