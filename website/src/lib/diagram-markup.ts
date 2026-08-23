// The markup contract for a rendered diagram, shared by the three places that
// have to agree on it: the build-time pipeline that emits the figure
// (vite-plugins/content.ts), the browser renderer that fills it in
// (src/lib/mermaid.ts), and the tests that assert on it. index.css styles the
// same class names.
export const DIAGRAM_FIGURE_CLASS = "mermaid-figure"
export const DIAGRAM_CANVAS_CLASS = "mermaid-canvas"
export const DIAGRAM_SOURCE_CLASS = "mermaid-source"

// Label on the <details> that holds the diagram source. It is the diagram's
// text alternative, and the whole diagram when JavaScript never runs.
export const DIAGRAM_SOURCE_LABEL = "Diagram source"

// `data-mermaid-state` on the figure: absent until a render pass has run, then
// one of these. CSS keys the canvas off it, so an unrendered figure takes no
// space instead of leaving an empty box.
export type DiagramState = "ready" | "failed" | "unavailable"
