import { useEffect, useSyncExternalStore, type ReactNode } from "react"
import { IntlProvider } from "react-intl"
import {
  getLocale,
  getServerLocale,
  initializeLocale,
  subscribeLocale,
} from "../../../../../ui/localization"
import { catalogs } from "./catalogs"

export function PortalIntlProvider({ children }: { children: ReactNode }) {
  const locale = useSyncExternalStore(
    subscribeLocale,
    getLocale,
    getServerLocale,
  )
  useEffect(initializeLocale, [])
  return (
    <IntlProvider
      locale={locale}
      defaultLocale="en"
      messages={catalogs[locale]}
    >
      {children}
    </IntlProvider>
  )
}
