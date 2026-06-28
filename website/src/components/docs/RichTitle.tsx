// Renders a nav/title string with `backtick` spans as inline monospace code.
export function RichTitle({ text }: { text: string }) {
  if (!text.includes("`")) return <>{text}</>
  const parts = text.split(/(`[^`]+`)/g)
  return (
    <>
      {parts.map((part, i) =>
        part.startsWith("`") && part.endsWith("`") && part.length > 1 ? (
          <code key={i} className="rounded bg-surface-tertiary px-1 py-0.5 font-mono text-[0.85em]">
            {part.slice(1, -1)}
          </code>
        ) : (
          <span key={i}>{part}</span>
        ),
      )}
    </>
  )
}
