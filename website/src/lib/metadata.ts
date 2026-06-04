import type { DocMeta } from "../../vite-plugins/content"

export type PageMetadataKind = "landing" | "doc" | "notFound"

export interface PageMetadata {
  kind: PageMetadataKind
  title: string
  description: string
  path: string
  canonicalUrl: string
  imageUrl: string
}

export const SITE_ORIGIN = "https://harnlang.com"
export const SITE_NAME = "Harn"
export const OG_IMAGE_PATH = "/og-default.png"
export const OG_IMAGE_URL = absoluteSiteUrl(OG_IMAGE_PATH)

export const LANDING_PAGE_META = pageMetadata({
  kind: "landing",
  title: "Harn — the pipeline-oriented language for AI agent orchestration",
  description:
    "Harn is a pipeline-oriented language for building, orchestrating, and operating AI agents. Tutorials, guides, and a complete language and runtime reference.",
  path: "/",
})

export const NOT_FOUND_PAGE_META = pageMetadata({
  kind: "notFound",
  title: `Page not found | ${SITE_NAME}`,
  description: "The requested Harn documentation page could not be found.",
  path: "/404.html",
})

export function pageMetaForDoc(doc: Pick<DocMeta, "title" | "description" | "url">): PageMetadata {
  return pageMetadata({
    kind: "doc",
    title: `${plainTitle(doc.title)} | ${SITE_NAME}`,
    description: doc.description,
    path: doc.url,
  })
}

function pageMetadata(input: Omit<PageMetadata, "canonicalUrl" | "imageUrl">): PageMetadata {
  return {
    ...input,
    canonicalUrl: absoluteSiteUrl(input.path),
    imageUrl: OG_IMAGE_URL,
  }
}

function absoluteSiteUrl(path: string): string {
  return new URL(path, SITE_ORIGIN).href
}

function plainTitle(title: string): string {
  return title.replace(/`([^`]+)`/g, "$1")
}
