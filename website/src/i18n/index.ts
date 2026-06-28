// Tiny, dependency-free i18n layer for the site.
//
// Today there is one locale (English). The shape below is what makes adding more
// cheap: write `fr.ts` mirroring `en.ts`, add it to `catalogs`, and resolve the
// locale per request (or from a context) in `useMessages` / `getMessages`.
import { en } from "./en"

export type Messages = typeof en
export type Locale = "en"

export const DEFAULT_LOCALE: Locale = "en"

const catalogs: Record<Locale, Messages> = { en }

/** Resolve a catalog. Use from non-React modules (metadata, gallery data). */
export function getMessages(locale: Locale = DEFAULT_LOCALE): Messages {
  return catalogs[locale] ?? catalogs[DEFAULT_LOCALE]
}

/**
 * React hook returning the active catalog. A future `LocaleProvider` can swap
 * the resolved locale here without touching call sites; it returns the default
 * locale on both server and client today, keeping SSR output stable.
 */
export function useMessages(): Messages {
  return getMessages()
}

/** Fill `{name}` placeholders in a message template. */
export function format(template: string, vars: Record<string, string | number>): string {
  return template.replace(/\{(\w+)\}/g, (_, key) =>
    key in vars ? String(vars[key]) : `{${key}}`,
  )
}
