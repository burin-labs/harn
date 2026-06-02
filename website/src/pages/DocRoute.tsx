import { useLocation } from "react-router"
import { meta } from "virtual:harn-docs"
import { slugFromPathname } from "../lib/page-store"
import { DocPage } from "./DocPage"
import { NotFound } from "./NotFound"

// Resolves the current `.html` path to a doc slug. Known slug → DocPage,
// otherwise the 404 page. Keyed on slug so DocPage gets fresh state per page.
export function DocRoute() {
  const location = useLocation()
  const slug = slugFromPathname(location.pathname)
  if (!meta[slug]) return <NotFound />
  return <DocPage key={slug} slug={slug} />
}
