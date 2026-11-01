import type { DocMeta } from "../../vite-plugins/content"
import { getMessages, format, DEFAULT_LOCALE, type Locale } from "../i18n"

export type PageMetadataKind = "landing" | "contribution" | "doc" | "notFound"

export interface PageMetadata {
  kind: PageMetadataKind
  title: string
  description: string
  path: string
  canonicalUrl: string
  imageUrl: string
}

export const SITE_ORIGIN = "https://harnlang.com"
export const SITE_NAME = getMessages(DEFAULT_LOCALE).common.siteName
export const OG_IMAGE_PATH = "/og-default.png"
export const OG_IMAGE_URL = absoluteSiteUrl(OG_IMAGE_PATH)

export const CONTRIBUTION_PATH = "/contribute.html"

export function landingPageMeta(locale: Locale = DEFAULT_LOCALE): PageMetadata {
  const t = getMessages(locale)
  return pageMetadata({
    kind: "landing",
    title: t.meta.landingTitle,
    description: t.meta.landingDescription,
    path: "/",
  })
}

export function notFoundPageMeta(locale: Locale = DEFAULT_LOCALE): PageMetadata {
  const t = getMessages(locale)
  return pageMetadata({
    kind: "notFound",
    title: format(t.meta.notFoundTitle, { siteName: SITE_NAME }),
    description: t.meta.notFoundDescription,
    path: "/404.html",
  })
}

export function contributionPageMeta(locale: Locale = DEFAULT_LOCALE): PageMetadata {
  const t = getMessages(locale)
  return pageMetadata({
    kind: "contribution",
    title: format(t.contribute.pageTitle, { siteName: SITE_NAME }),
    description: t.contribute.intro,
    path: CONTRIBUTION_PATH,
  })
}

export function staticPageMetadata(): PageMetadata[] {
  return [landingPageMeta(), contributionPageMeta()]
}

export function pageMetaForDoc(doc: Pick<DocMeta, "title" | "description" | "url">, locale: Locale = DEFAULT_LOCALE): PageMetadata {
  const t = getMessages(locale)
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
