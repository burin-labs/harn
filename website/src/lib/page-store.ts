import type { PageData } from "../../vite-plugins/content"

// A tiny per-slug cache for rendered doc pages. The current page is seeded
// synchronously (from the inlined SSR payload on first load, or from the
// prerender on the server) so the first render needs no fetch and hydrates
// without mismatch. Subsequent client navigations fetch `/_content/<slug>.json`.
const cache = new Map<string, PageData>()

export function seedPage(data: PageData | null | undefined): void {
  if (data && typeof data.slug === "string") cache.set(data.slug, data)
}

export function getCachedPage(slug: string): PageData | null {
  return cache.get(slug) ?? null
}

export async function fetchPage(slug: string): Promise<PageData | null> {
  const cached = cache.get(slug)
  if (cached) return cached
  try {
    const res = await fetch(`/_content/${slug}.json`)
    if (!res.ok) return null
    const data = (await res.json()) as PageData
    if (!data || typeof data.slug !== "string") return null
    cache.set(slug, data)
    return data
  } catch {
    return null
  }
}

export function slugFromPathname(pathname: string): string {
  return decodeURIComponent(pathname)
    .replace(/^\/+/, "")
    .replace(/\.html$/, "")
}
