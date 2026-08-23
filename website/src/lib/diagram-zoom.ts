// Full-screen zoom for a rendered diagram.
//
// A flowchart that fits the article column is often too small to read: Mermaid
// scales the SVG down to the available width, and a wide left-to-right chart
// loses its labels first. This opens the same SVG in a modal overlay where it
// can be scaled and panned.
//
// Built on a native <dialog> with showModal(), which supplies the focus trap,
// the inert background, Escape-to-close, and focus restoration to the element
// that opened it. The diagram is vector, so zooming is a CSS transform on a
// clone — there is nothing to re-render and nothing to fetch.

import { getMessages } from "../i18n"

const MIN_SCALE = 0.2
const MAX_SCALE = 8
// A small diagram should be allowed to fill the screen — that is the point of
// opening it — but not to the degree that a three-node chart becomes a mural.
const MAX_FIT_SCALE = 4
const ZOOM_STEP = 1.25

interface Overlay {
  dialog: HTMLDialogElement
  stage: HTMLElement
  figure: HTMLElement
  percent: HTMLElement
}

interface ViewState {
  scale: number
  fitScale: number
  x: number
  y: number
}

let overlay: Overlay | null = null
const view: ViewState = { scale: 1, fitScale: 1, x: 0, y: 0 }

function naturalSize(svg: SVGElement): { width: number; height: number } {
  const viewBox = svg.getAttribute("viewBox")?.split(/[\s,]+/).map(Number)
  if (viewBox && viewBox.length === 4 && viewBox[2] > 0 && viewBox[3] > 0) {
    return { width: viewBox[2], height: viewBox[3] }
  }
  const box = svg.getBoundingClientRect()
  return { width: box.width || 640, height: box.height || 480 }
}

function applyView() {
  if (!overlay) return
  overlay.figure.style.transform = `translate(${view.x}px, ${view.y}px) scale(${view.scale})`
  overlay.percent.textContent = `${Math.round((view.scale / view.fitScale) * 100)}%`
}

function setScale(next: number, origin?: { x: number; y: number }) {
  if (!overlay) return
  const clamped = Math.min(MAX_SCALE, Math.max(MIN_SCALE, next))
  if (clamped === view.scale) return
  if (origin) {
    // Keep the point under the cursor fixed while the scale changes.
    const rect = overlay.stage.getBoundingClientRect()
    const cx = origin.x - rect.left - rect.width / 2 - view.x
    const cy = origin.y - rect.top - rect.height / 2 - view.y
    const ratio = clamped / view.scale
    view.x -= cx * (ratio - 1)
    view.y -= cy * (ratio - 1)
  }
  view.scale = clamped
  applyView()
}

function resetView() {
  view.scale = view.fitScale
  view.x = 0
  view.y = 0
  applyView()
}

function button(label: string, text: string, onClick: () => void): HTMLButtonElement {
  const el = document.createElement("button")
  el.type = "button"
  el.className = "diagram-overlay-button"
  el.setAttribute("aria-label", label)
  el.textContent = text
  el.addEventListener("click", onClick)
  return el
}

function createOverlay(): Overlay {
  const t = getMessages().diagram
  const dialog = document.createElement("dialog")
  dialog.className = "diagram-overlay"
  dialog.setAttribute("aria-label", t.overlayAria)

  const bar = document.createElement("div")
  bar.className = "diagram-overlay-bar"

  const percent = document.createElement("span")
  percent.className = "diagram-overlay-percent"
  // Announce the zoom level to assistive tech as it changes.
  percent.setAttribute("aria-live", "polite")

  bar.append(
    button(t.zoomOut, "−", () => setScale(view.scale / ZOOM_STEP)),
    percent,
    button(t.zoomIn, "+", () => setScale(view.scale * ZOOM_STEP)),
    button(t.resetZoom, t.resetZoomLabel, resetView),
    button(t.close, "✕", () => dialog.close()),
  )

  const stage = document.createElement("div")
  stage.className = "diagram-overlay-stage"

  const figure = document.createElement("div")
  figure.className = "diagram-overlay-figure"
  stage.append(figure)
  dialog.append(bar, stage)
  document.body.append(dialog)

  // Wheel zooms rather than scrolls; the stage has nothing else to scroll.
  stage.addEventListener(
    "wheel",
    (event) => {
      event.preventDefault()
      const factor = event.deltaY < 0 ? ZOOM_STEP : 1 / ZOOM_STEP
      setScale(view.scale * factor, { x: event.clientX, y: event.clientY })
    },
    { passive: false },
  )

  let dragging = false
  let lastX = 0
  let lastY = 0
  stage.addEventListener("pointerdown", (event) => {
    dragging = true
    lastX = event.clientX
    lastY = event.clientY
    stage.setPointerCapture(event.pointerId)
    stage.classList.add("is-panning")
  })
  stage.addEventListener("pointermove", (event) => {
    if (!dragging) return
    view.x += event.clientX - lastX
    view.y += event.clientY - lastY
    lastX = event.clientX
    lastY = event.clientY
    applyView()
  })
  const endDrag = () => {
    dragging = false
    stage.classList.remove("is-panning")
  }
  stage.addEventListener("pointerup", endDrag)
  stage.addEventListener("pointercancel", endDrag)

  stage.addEventListener("dblclick", (event) => {
    const target = view.scale > view.fitScale * 1.05 ? view.fitScale : view.fitScale * 2
    setScale(target, { x: event.clientX, y: event.clientY })
  })

  // Clicking the backdrop closes, matching every other lightbox. A click that
  // landed on the diagram or the toolbar is not a backdrop click.
  dialog.addEventListener("click", (event) => {
    if (event.target === dialog || event.target === stage) dialog.close()
  })

  dialog.addEventListener("keydown", (event) => {
    if (event.key === "+" || event.key === "=") {
      event.preventDefault()
      setScale(view.scale * ZOOM_STEP)
    } else if (event.key === "-") {
      event.preventDefault()
      setScale(view.scale / ZOOM_STEP)
    } else if (event.key === "0") {
      event.preventDefault()
      resetView()
    }
  })

  dialog.addEventListener("close", () => {
    figure.replaceChildren()
  })

  return { dialog, stage, figure, percent }
}

/** The id the overlay's copy of `sourceId`'s diagram is given. */
export function zoomIdFor(sourceId: string): string {
  return `${sourceId}-zoom`
}

/**
 * Repoint a Mermaid diagram's embedded stylesheet from one SVG id to another.
 *
 * Mermaid scopes every rule it emits to the SVG's own id
 * (`#mermaid-3 .node rect { … }`) in a <style> inside the SVG. A copy therefore
 * needs a new id — two elements cannot share one — and its rules have to follow,
 * or the copy renders with browser defaults: black fills and unstyled labels.
 */
export function rescopeStyles(css: string, fromId: string, toId: string): string {
  // Anchored on a non-identifier boundary: a plain substring swap would rewrite
  // `#mermaid-10` while rescoping `#mermaid-1`.
  const escaped = fromId.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
  return css.replace(new RegExp(`#${escaped}(?![\\w-])`, "g"), `#${toId}`)
}

/** Open `svg` (a clone is taken) in the zoom overlay. */
export function openDiagram(svg: SVGElement) {
  overlay ??= createOverlay()
  const clone = svg.cloneNode(true) as SVGElement
  const { width, height } = naturalSize(svg)

  // Mermaid sizes its SVG to the container it rendered into. In the overlay the
  // wrapper owns the size, so the clone goes back to its intrinsic dimensions.
  clone.removeAttribute("style")
  clone.setAttribute("width", String(width))
  clone.setAttribute("height", String(height))

  const sourceId = svg.getAttribute("id")
  if (sourceId) {
    const cloneId = zoomIdFor(sourceId)
    clone.setAttribute("id", cloneId)
    for (const style of clone.querySelectorAll("style")) {
      style.textContent = rescopeStyles(style.textContent ?? "", sourceId, cloneId)
    }
  }

  overlay.figure.replaceChildren(clone)
  overlay.figure.style.width = `${width}px`
  overlay.figure.style.height = `${height}px`

  overlay.dialog.showModal()

  // showModal() must run before measuring: the stage has no size until then.
  const stage = overlay.stage.getBoundingClientRect()
  view.fitScale = Math.min(
    MAX_FIT_SCALE,
    Math.min((stage.width * 0.94) / width, (stage.height * 0.94) / height),
  )
  resetView()
}
