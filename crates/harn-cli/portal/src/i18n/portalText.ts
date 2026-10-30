import { createIntl, createIntlCache, type IntlShape } from "react-intl"
import { getLocale, type Locale } from "../../../../../ui/localization"
import { catalogs } from "./catalogs"

const cache = createIntlCache()
const instances = new Map<Locale, IntlShape>()

export function portalText(
  message: string,
  values?: Record<string, string | number | boolean | null | undefined>,
): string {
  const locale = getLocale()
  let intl = instances.get(locale)
  if (!intl) {
    intl = createIntl(
      { locale, defaultLocale: "en", messages: catalogs[locale] },
      cache,
    )
    instances.set(locale, intl)
  }
  const printable =
    values &&
    Object.fromEntries(
      Object.entries(values).map(([key, value]) => [
        key,
        value == null || typeof value === "boolean" ? "" : value,
      ]),
    )
  return intl.formatMessage({ id: message, defaultMessage: message }, printable)
}
