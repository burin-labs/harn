import { useCallback, useEffect, useState } from "react"
import { Outlet, useLocation } from "react-router"
import { Navbar } from "../components/Navbar"
import { PrereleaseBanner } from "../components/PrereleaseBanner"
import { Footer } from "../components/Footer"
import { SearchModal } from "../components/SearchModal"
import { KeywordTooltip } from "../components/KeywordTooltip"

export function RootLayout() {
  const [searchOpen, setSearchOpen] = useState(false)
  const location = useLocation()
  const openSearch = useCallback(() => setSearchOpen(true), [])
  const closeSearch = useCallback(() => setSearchOpen(false), [])

  // ⌘K / Ctrl-K opens search anywhere.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault()
        setSearchOpen((v) => !v)
      }
    }
    window.addEventListener("keydown", onKey)
    return () => window.removeEventListener("keydown", onKey)
  }, [])

  // Scroll to top (or to the hash anchor) on navigation.
  useEffect(() => {
    if (location.hash) {
      const el = document.getElementById(decodeURIComponent(location.hash.slice(1)))
      if (el) {
        el.scrollIntoView()
        return
      }
    }
    window.scrollTo(0, 0)
  }, [location.pathname, location.hash])

  return (
    <div className="flex min-h-screen flex-col">
      <PrereleaseBanner />
      <Navbar onOpenSearch={openSearch} />
      <main className="flex-1">
        <Outlet />
      </main>
      <Footer />
      <SearchModal open={searchOpen} onClose={closeSearch} />
      <KeywordTooltip />
    </div>
  )
}
