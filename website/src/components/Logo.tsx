// Harn mark — a lowercase h read as a pipeline: the stem is the program, the
// shoulder is the branch that spends a model call, and the joint at its foot is
// what the branch hands back.
//
// The gradient is declared in userSpaceOnUse, not the default objectBoundingBox.
// The stem is a straight vertical line, so its bounding box has zero width, and
// an objectBoundingBox paint server on a zero-area box does not render at all.
// The previous mark hit exactly that: its horizontal rail never painted, which
// is why its nodes looked unconnected.
//
// The stops are a teal-to-amber ramp interpolated in OKLCH rather than sRGB.
// A straight sRGB line between two hues this far apart passes near the
// achromatic axis and goes olive in the middle; OKLCH keeps chroma up across
// the ramp. Kept in step with public/favicon.svg, which carries the same
// silhouette flattened for small sizes.
export function Logo({ className = "h-8 w-8" }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 32 32" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
      <defs>
        <linearGradient id="harn-flow" gradientUnits="userSpaceOnUse" x1="8" y1="6" x2="24" y2="26">
          <stop offset="0%" stopColor="#14b8a6" />
          <stop offset="17%" stopColor="#40bc8d" />
          <stop offset="33%" stopColor="#6bbd6f" />
          <stop offset="50%" stopColor="#92ba4d" />
          <stop offset="67%" stopColor="#b8b325" />
          <stop offset="83%" stopColor="#d9a900" />
          <stop offset="100%" stopColor="#f59e0b" />
        </linearGradient>
      </defs>
      <g
        fill="none"
        stroke="url(#harn-flow)"
        strokeWidth="2.9"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        <path d="M9.5 6 V25.5" />
        <path d="M9.5 18 Q9.5 12.4 16 12.4 Q22.5 12.4 22.5 18 V25.5" />
      </g>
      <circle cx="22.5" cy="25.5" r="2" fill="#f59e0b" />
    </svg>
  )
}
