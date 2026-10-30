import en from "./en.json"
import es from "./es.json"
import fr from "./fr.json"
import de from "./de.json"
import ptBR from "./pt-BR.json"
import it from "./it.json"
import ja from "./ja.json"
import ko from "./ko.json"
import zhHans from "./zh-Hans.json"
import type { Locale } from "../../../../../ui/localization"

export const catalogs: Record<Locale, Record<keyof typeof en, string>> = {
  en,
  es,
  fr,
  de,
  "pt-BR": ptBR,
  it,
  ja,
  ko,
  "zh-Hans": zhHans,
}
