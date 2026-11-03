export const languages = {
  en: "English",
  es: "Español",
  fr: "Français",
  de: "Deutsch",
  "pt-BR": "Português (Brasil)",
  it: "Italiano",
  ja: "日本語",
  ko: "한국어",
  "zh-Hans": "简体中文",
  ru: "Русский",
} as const

export type Locale = keyof typeof languages
const storageKey = "harn.language"
const listeners = new Set<() => void>()
let current: Locale = "en"
let initialized = false

export function resolveLocale(value: string): Locale | undefined {
  let parsed: Intl.Locale
  try {
    parsed = new Intl.Locale(value.replaceAll("_", "-"))
  } catch {
    return undefined
  }
  const base = parsed.language
  if (base === "zh") {
    if (parsed.script) return parsed.script === "Hans" ? "zh-Hans" : undefined
    return ["TW", "HK", "MO"].includes(parsed.region ?? "") ? undefined : "zh-Hans"
  }
  if (base === "pt") return "pt-BR"
  return Object.hasOwn(languages, base) ? (base as Locale) : undefined
}

export function getLocale(): Locale {
  return current
}
export function getServerLocale(): Locale {
  return "en"
}
export function subscribeLocale(listener: () => void): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

export function setLocale(locale: Locale): void {
  current = locale
  if (typeof document !== "undefined") document.documentElement.lang = locale
  try {
    localStorage.setItem(storageKey, locale)
  } catch {
    /* Storage can be disabled. */
  }
  listeners.forEach((listener) => listener())
}

export function initializeLocale(): void {
  if (initialized || typeof navigator === "undefined") return
  initialized = true
  let saved: string | null = null
  try {
    saved = localStorage.getItem(storageKey)
  } catch {
    /* Use browser language. */
  }
  const locale =
    (saved && resolveLocale(saved)) ||
    navigator.languages.map(resolveLocale).find(Boolean) ||
    "en"
  setLocale(locale)
}
