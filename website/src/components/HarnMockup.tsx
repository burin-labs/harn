import type { ReactNode } from "react"
import heroSnippetRaw from "../examples/hero.harn.txt?raw"
import { highlightHarnSource } from "../lib/harn-highlight"

// Hero mockup — a Harn pipeline source pane beside a live agent run, echoing the
// Burin Code editor mockup but expressing what Harn itself is: orchestration code.
export const heroSnippetSource = heroSnippetRaw.trimEnd()

function ChatRow({
  tone,
  children,
}: {
  tone: "user" | "agent" | "thought" | "tool"
  children: ReactNode
}) {
  const styles: Record<typeof tone, string> = {
    user: "border border-white/10 bg-white/[0.04] text-white/85",
    agent: "border border-accent-500/30 bg-accent-500/10 text-accent-100",
    thought: "italic text-white/45",
    tool: "border border-white/10 bg-[#11201d] font-mono text-[11px] text-emerald-300/90",
  }
  return <div className={`rounded-lg px-3 py-2 ${styles[tone]}`}>{children}</div>
}

export function HarnMockup() {
  return (
    <div
      role="img"
      aria-label="A Harn pipeline definition running as a live agent"
      className="relative mx-auto max-w-4xl"
    >
      <div className="absolute -inset-x-8 -inset-y-6 rounded-[2rem] bg-gradient-to-r from-accent-500/20 via-accent-400/10 to-amber-400/20 blur-2xl" />
      <div className="relative overflow-hidden rounded-2xl border border-white/10 bg-[#070f0e] shadow-2xl shadow-accent-900/40 ring-1 ring-white/5">
        <div className="flex items-center gap-2 border-b border-white/5 bg-[#0c1413] px-4 py-3">
          <span className="h-3 w-3 rounded-full bg-[#ff5f57]" />
          <span className="h-3 w-3 rounded-full bg-[#febc2e]" />
          <span className="h-3 w-3 rounded-full bg-[#28c840]" />
          <span className="ml-3 font-mono text-xs font-medium text-white/50">review.harn</span>
        </div>
        <div className="grid grid-cols-12 text-[13px] leading-relaxed">
          <div className="col-span-12 px-5 py-4 font-mono text-[12.5px] sm:col-span-7">
            <pre className="m-0 whitespace-pre text-white/80">
              <code data-hero-snippet="true" className="language-harn">{highlightHarnSource(heroSnippetSource)}</code>
              <span className="ml-px inline-block h-3 w-1.5 -translate-y-px bg-accent-400 align-middle" />
            </pre>
          </div>
          <div className="col-span-12 border-t border-white/5 bg-[#060c0b] sm:col-span-5 sm:border-t-0 sm:border-l">
            <div className="flex items-center gap-2 border-b border-white/5 px-4 py-2 text-[11px] uppercase tracking-wider text-white/40">
              <span className="h-1.5 w-1.5 rounded-full bg-accent-400 animate-pulse-dot" />
              harn run
            </div>
            <div className="space-y-3 px-4 py-3 text-[12px] leading-relaxed">
              <ChatRow tone="thought">Reading diff for crates/harn-vm…</ChatRow>
              <ChatRow tone="tool">llm_call · claude-opus-4-8 · 2 tools</ChatRow>
              <ChatRow tone="agent">
                Flagged 3 issues: an unguarded unwrap in <code>call_closure</code>, a missing
                bounds check, and one stale doc snippet.
              </ChatRow>
              <ChatRow tone="tool">spawn agent · deadline 30s</ChatRow>
            </div>
          </div>
        </div>
        <div className="flex items-center justify-between border-t border-white/5 bg-[#0c1413] px-4 py-1.5 text-[11px] text-white/40">
          <span>deterministic replay on</span>
          <span className="flex items-center gap-1.5">
            <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
            3 findings
          </span>
        </div>
      </div>
    </div>
  )
}
