import { useEffect } from "react"
import { useLocale, type Locale } from "../i18n"
import type { PageMetadata } from "../lib/metadata"

export function usePageMetadata(resolve: (locale: Locale) => PageMetadata): void {
  const { title, description } = resolve(useLocale())
  useEffect(() => {
    document.title = title
    let element = document.querySelector<HTMLMetaElement>('meta[name="description"]')
    if (!element) {
      element = document.createElement("meta")
      element.name = "description"
      document.head.append(element)
    }
    element.content = description
  }, [title, description])
}
