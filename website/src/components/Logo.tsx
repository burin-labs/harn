// Harn mark — a pipeline glyph: three linked stages flowing left to right,
// rendered in the teal→amber brand gradient.
export function Logo({ className = "h-8 w-8" }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 32 32" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
      <defs>
        <linearGradient id="harn-bg" x1="0%" y1="0%" x2="100%" y2="100%">
          <stop offset="0%" stopColor="#0b1413" />
          <stop offset="100%" stopColor="#13201e" />
        </linearGradient>
        <linearGradient id="harn-flow" x1="0%" y1="50%" x2="100%" y2="50%">
          <stop offset="0%" stopColor="#5eead4" />
          <stop offset="55%" stopColor="#14b8a6" />
          <stop offset="100%" stopColor="#f59e0b" />
        </linearGradient>
      </defs>
      <rect width="32" height="32" rx="7" fill="url(#harn-bg)" />
      <path
        d="M7 16 H25"
        stroke="url(#harn-flow)"
        strokeWidth="2.2"
        strokeLinecap="round"
        fill="none"
      />
      <circle cx="7.5" cy="16" r="3" fill="url(#harn-flow)" />
      <circle cx="16" cy="16" r="3" fill="url(#harn-flow)" />
      <circle cx="24.5" cy="16" r="3" fill="url(#harn-flow)" />
      <path
        d="M16 16 L11.5 10.5 M16 16 L20.5 21.5"
        stroke="url(#harn-flow)"
        strokeWidth="2.2"
        strokeLinecap="round"
        fill="none"
        opacity="0.85"
      />
      <circle cx="11.2" cy="10.2" r="2.4" fill="#5eead4" />
      <circle cx="20.8" cy="21.8" r="2.4" fill="#f59e0b" />
    </svg>
  )
}
