import type { DocMeta } from "../../vite-plugins/content"
import { getMessages, format } from "../i18n"

const t = getMessages()

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
export const SITE_NAME = t.common.siteName
export const OG_IMAGE_PATH = "/og-default.png"
export const OG_IMAGE_URL = absoluteSiteUrl(OG_IMAGE_PATH)

export const LANDING_PAGE_META = pageMetadata({
  kind: "landing",
  title: t.meta.landingTitle,
  description: t.meta.landingDescription,
  path: "/",
})

export const NOT_FOUND_PAGE_META = pageMetadata({
  kind: "notFound",
  title: format(t.meta.notFoundTitle, { siteName: SITE_NAME }),
  description: t.meta.notFoundDescription,
  path: "/404.html",
})

export function pageMetaForDoc(doc: Pick<DocMeta, "title" | "description" | "url">): PageMetadata {
  return pageMetadata({
    kind: "doc",
    title: format(t.meta.docTitle, { title: plainTitle(doc.title), siteName: SITE_NAME }),
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
