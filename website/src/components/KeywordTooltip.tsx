import { useEffect, useState } from "react"
import { createPortal } from "react-dom"

type Tip = { text: string; x: number; y: number } | null

// A single, page-wide hover tooltip for code tokens tagged with `data-harn-tip`
// by the Harn highlighter. One delegated listener for the whole document and one
// portaled element, so adding tooltips to a snippet costs no extra JS, and the
// fixed-positioned bubble escapes the scrollable/overflow-clipped code panes.
//
// Pointer devices only: touch users get no hover affordance (and no wasted work).
export function KeywordTooltip() {
  const [tip, setTip] = useState<Tip>(null)

  useEffect(() => {
    if (typeof window === "undefined") return
    if (!window.matchMedia?.("(hover: hover)").matches) return

    const show = (event: Event) => {
      const target = event.target as HTMLElement | null
      const el = target?.closest?.("[data-harn-tip]") as HTMLElement | null
      if (!el) return
      const text = el.getAttribute("data-harn-tip")
      if (!text) return
      const rect = el.getBoundingClientRect()
      const x = Math.min(Math.max(rect.left + rect.width / 2, 160), window.innerWidth - 160)
      setTip({ text, x, y: rect.top })
    }
    const hide = (event: Event) => {
      const target = event.target as HTMLElement | null
      if (target?.closest?.("[data-harn-tip]")) setTip(null)
    }

    document.addEventListener("mouseover", show)
    document.addEventListener("mouseout", hide)
    window.addEventListener("scroll", () => setTip(null), true)
    return () => {
      document.removeEventListener("mouseover", show)
      document.removeEventListener("mouseout", hide)
    }
  }, [])

  if (!tip) return null
  return createPortal(
    <div
      role="tooltip"
      className="pointer-events-none fixed z-[60] max-w-xs -translate-x-1/2 -translate-y-full rounded-lg border border-border bg-surface-elevated px-3 py-2 text-xs leading-snug text-foreground-secondary shadow-lg"
      style={{ left: tip.x, top: tip.y - 8 }}
    >
      {tip.text}
    </div>,
    document.body,
  )
}
