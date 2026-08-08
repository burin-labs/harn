export function formatDuration(ms: number | null | undefined): string {
  if (ms == null) {return "n/a"}
  if (ms >= 60000) {return `${(ms / 60000).toFixed(1)}m`}
  if (ms >= 1000) {return `${(ms / 1000).toFixed(1)}s`}
  return `${ms}ms`
}

export function formatNumber(n: number | null | undefined): string {
  if (n == null) {return "0"}
  if (n >= 1_000_000) {return `${(n / 1_000_000).toFixed(1)}M`}
  if (n >= 1_000) {return `${(n / 1_000).toFixed(1)}K`}
  return String(n)
}

export function statusClass(status: string): string {
  if (["complete", "completed", "success", "verified"].includes(status)) {return "complete"}
  if (["failed", "error", "cancelled"].includes(status)) {return status}
  return "running"
}

export function pct(value: number, total: number): string {
  return ((value / Math.max(total, 1)) * 100).toFixed(3)
}
