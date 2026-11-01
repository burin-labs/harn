import { useSyncExternalStore } from "react"
import { getLocale, getServerLocale, subscribeLocale, type Locale } from "../../../ui/localization"
import { en } from "./en"
import es from "./es.json"
import fr from "./fr.json"
import de from "./de.json"
import ptBR from "./pt-BR.json"
import it from "./it.json"
import ja from "./ja.json"
import ko from "./ko.json"
import zhHans from "./zh-Hans.json"
import ru from "./ru.json"

export type Messages = typeof en
export type { Locale }

export const DEFAULT_LOCALE: Locale = "en"

export const catalogs: Record<Locale, Messages> = { en, es, fr, de, "pt-BR": ptBR, it, ja, ko, "zh-Hans": zhHans, ru }

/** Resolve a catalog. Use from non-React modules (metadata, gallery data). */
export function getMessages(locale: Locale = getLocale()): Messages {
  return catalogs[locale] ?? catalogs[DEFAULT_LOCALE]
}

export function useMessages(): Messages {
  return getMessages(useLocale())
}

export function useLocale(): Locale {
  return useSyncExternalStore(subscribeLocale, getLocale, getServerLocale)
}

/** Fill `{name}` placeholders in a message template. */
export function format(template: string, vars: Record<string, string | number>): string {
  return template.replace(/\{(\w+)\}/g, (_, key) =>
    key in vars ? String(vars[key]) : `{${key}}`,
  )
}
